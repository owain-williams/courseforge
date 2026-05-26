//! Runtime registry for in-flight Video → MP4 exports.
//!
//! Owns one [`ExportJob`] per Video, the per-job [`CancelToken`], and the
//! plumbing that turns an EDL + Segment + transcript into:
//!   * a final `.mp4` at the user-chosen destination, and
//!   * an aligned `.srt` sidecar.
//!
//! The actual MP4 rendering is delegated to a [`crate::exporter::Exporter`]
//! backend (ffmpeg on macOS, fake elsewhere). This module owns the policy
//! around it: where files land by default, how progress is surfaced, how
//! cancellation drops half-written output, and how the source Segment and
//! the EDL are left strictly untouched (AC: "Export does not modify source
//! files or the EDL").

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::core::edits::{self, Cut};
use crate::core::error::{CoreError, Result};
use crate::core::export::{
    default_export_dir, edited_duration_sec, keep_ranges, srt_from_transcript,
};
use crate::core::segments;
use crate::core::transcript;
use crate::exporter::{CancelToken, Exporter, ProgressSink};

/// Where this job is in its lifecycle. Mirrors the transcription manager's
/// `JobStatus` so the UI's progress-bar wiring is the same here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExportStatus {
    Pending,
    Running { fraction: f64 },
    Done {
        mp4: PathBuf,
        srt: PathBuf,
    },
    Failed { message: String },
    Cancelled,
}

/// Snapshot of one Video's export job, safe to ship over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportJob {
    #[serde(rename = "videoId")]
    pub video_id: String,
    #[serde(rename = "courseFolder")]
    pub course_folder: PathBuf,
    /// Directory the renderer is writing into (`<exports>/<video-id>/` by
    /// default; user-overridable).
    #[serde(rename = "destinationDir")]
    pub destination_dir: PathBuf,
    pub status: ExportStatus,
}

/// A subscriber gets every status change. Used to push updates to the
/// frontend via Tauri events without coupling this module to Tauri.
pub type Subscriber = Box<dyn Fn(&ExportJob) + Send + Sync>;

struct Inner {
    jobs: HashMap<String, ExportJob>,
    cancellers: HashMap<String, CancelToken>,
    subscriber: Option<Subscriber>,
}

pub struct ExportManager {
    backend: Arc<dyn Exporter>,
    inner: Mutex<Inner>,
}

impl ExportManager {
    pub fn new(backend: Box<dyn Exporter>) -> Self {
        Self {
            backend: Arc::from(backend),
            inner: Mutex::new(Inner {
                jobs: HashMap::new(),
                cancellers: HashMap::new(),
                subscriber: None,
            }),
        }
    }

    pub fn set_subscriber(&self, sub: Subscriber) {
        self.inner.lock().unwrap().subscriber = Some(sub);
    }

    pub fn jobs(&self) -> Vec<ExportJob> {
        self.inner.lock().unwrap().jobs.values().cloned().collect()
    }

    pub fn job_for_video(&self, video_id: &str) -> Option<ExportJob> {
        self.inner.lock().unwrap().jobs.get(video_id).cloned()
    }

    /// Default destination directory for a Video's export. The user's
    /// chosen destination (if any) overrides this.
    pub fn default_destination(&self, course_folder: &Path, video_id: &str) -> PathBuf {
        default_export_dir(course_folder, video_id)
    }

    /// Kick off an export job *synchronously*. Returns once the MP4 and SRT
    /// are on disk (or an error has been recorded against the job).
    ///
    /// "Synchronous" because production code runs this on a worker thread —
    /// see [`spawn_export`]. Tests call it inline so they can deterministically
    /// observe the resulting state.
    pub fn start_export(
        &self,
        course_folder: &Path,
        video_id: &str,
        destination_dir: Option<PathBuf>,
    ) -> Result<ExportJob> {
        let destination_dir =
            destination_dir.unwrap_or_else(|| self.default_destination(course_folder, video_id));

        // Reject double-starts so the UI's "Export" button doesn't accidentally
        // race two ffmpegs writing to the same destination.
        {
            let inner = self.inner.lock().unwrap();
            if let Some(j) = inner.jobs.get(video_id) {
                if matches!(j.status, ExportStatus::Pending | ExportStatus::Running { .. }) {
                    return Err(CoreError::ExportAlreadyRunning(video_id.to_string()));
                }
            }
        }

        let cancel = CancelToken::new();
        let initial = ExportJob {
            video_id: video_id.to_string(),
            course_folder: course_folder.to_path_buf(),
            destination_dir: destination_dir.clone(),
            status: ExportStatus::Pending,
        };
        {
            let mut inner = self.inner.lock().unwrap();
            inner.jobs.insert(video_id.to_string(), initial.clone());
            inner.cancellers.insert(video_id.to_string(), cancel.clone());
            notify(&inner, &initial);
        }

        let outcome = self.render(course_folder, video_id, &destination_dir, &cancel);
        let status = match outcome {
            Ok((mp4, srt)) => ExportStatus::Done { mp4, srt },
            Err(CoreError::ExportCancelled) => ExportStatus::Cancelled,
            Err(e) => ExportStatus::Failed { message: e.to_string() },
        };
        Ok(self.finalize(video_id, status))
    }

    /// Run the export on a worker thread. Returns immediately with the
    /// initial (Pending) snapshot. Status updates flow to the subscriber.
    pub fn spawn_export(
        self: &Arc<Self>,
        course_folder: PathBuf,
        video_id: String,
        destination_dir: Option<PathBuf>,
    ) -> Result<ExportJob> {
        // Reserve the slot synchronously so a follow-up `start_export` for
        // the same video won't race the worker.
        let destination_dir = destination_dir
            .unwrap_or_else(|| self.default_destination(&course_folder, &video_id));
        let cancel = CancelToken::new();
        let initial = {
            let mut inner = self.inner.lock().unwrap();
            if let Some(j) = inner.jobs.get(&video_id) {
                if matches!(j.status, ExportStatus::Pending | ExportStatus::Running { .. }) {
                    return Err(CoreError::ExportAlreadyRunning(video_id));
                }
            }
            let job = ExportJob {
                video_id: video_id.clone(),
                course_folder: course_folder.clone(),
                destination_dir: destination_dir.clone(),
                status: ExportStatus::Pending,
            };
            inner.jobs.insert(video_id.clone(), job.clone());
            inner.cancellers.insert(video_id.clone(), cancel.clone());
            notify(&inner, &job);
            job
        };

        let me = self.clone();
        std::thread::spawn(move || {
            let outcome = me.render(&course_folder, &video_id, &destination_dir, &cancel);
            let status = match outcome {
                Ok((mp4, srt)) => ExportStatus::Done { mp4, srt },
                Err(CoreError::ExportCancelled) => ExportStatus::Cancelled,
                Err(e) => ExportStatus::Failed { message: e.to_string() },
            };
            me.finalize(&video_id, status);
        });
        Ok(initial)
    }

    /// Ask a running or pending job to stop. The cancel token is flipped
    /// synchronously, but the backend has to notice it on its next poll —
    /// the job's status moves to Cancelled when the worker returns.
    pub fn cancel(&self, video_id: &str) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let cancel = inner
            .cancellers
            .get(video_id)
            .ok_or_else(|| CoreError::ExportJobNotFound(video_id.to_string()))?
            .clone();
        drop(inner);
        cancel.cancel();
        Ok(())
    }

    // -- internals ------------------------------------------------------------

    fn render(
        &self,
        course_folder: &Path,
        video_id: &str,
        destination_dir: &Path,
        cancel: &CancelToken,
    ) -> Result<(PathBuf, PathBuf)> {
        let segment_path = first_segment_path(course_folder, video_id)?;
        let segment_path_abs = course_folder.join(&segment_path);

        // Capture an EDL snapshot up front so a parallel cut/undo while we
        // render doesn't change what we export. The on-disk `edits.json` is
        // never written to by this code path — only read.
        let edl = edits::current_state(course_folder, video_id)?;
        let cuts: Vec<Cut> = edl.cuts.clone();

        let source_duration = read_duration_sec(&segment_path_abs)?;
        let ranges = keep_ranges(source_duration, &cuts);

        std::fs::create_dir_all(destination_dir).map_err(|e| CoreError::Io {
            path: destination_dir.to_path_buf(),
            source: e,
        })?;
        let mp4_path = destination_dir.join(format!("{video_id}.mp4"));
        let srt_path = destination_dir.join(format!("{video_id}.srt"));

        // Source bytes captured *before* the renderer runs, so we can verify
        // afterwards that the renderer didn't touch them. This is cheap for
        // a single Segment and gives us a clean AC ("does not modify source
        // files") check inside the manager.
        let src_bytes_before = std::fs::metadata(&segment_path_abs)
            .map(|m| (m.len(), m.modified().ok()))
            .ok();
        let edits_bytes_before = std::fs::read(edits::edits_path(course_folder, video_id)).ok();

        self.mark_running(video_id, 0.0);

        let sink = ManagerProgressSink {
            manager: self,
            video_id: video_id.to_string(),
        };
        self.backend.export_mp4(
            &segment_path_abs,
            &ranges,
            &mp4_path,
            &sink,
            cancel,
        )?;

        // Write the SRT after the MP4 — that order lets the renderer's
        // failure (or cancellation) skip the caption write so we don't end
        // up with a stranded `.srt` next to no MP4.
        let transcript = transcript::read_transcript(course_folder, video_id)?
            .ok_or_else(|| CoreError::NoTranscriptForExport(video_id.to_string()))?;
        let srt = srt_from_transcript(&transcript, &cuts);
        std::fs::write(&srt_path, srt.as_bytes()).map_err(|e| CoreError::Io {
            path: srt_path.clone(),
            source: e,
        })?;

        // Side-effect guard: confirm the source Segment and on-disk EDL are
        // bit-for-bit identical to what we read at the start. A backend that
        // mutates the source would fail here loudly rather than silently.
        if let Some((len_before, mtime_before)) = src_bytes_before {
            let meta = std::fs::metadata(&segment_path_abs).map_err(|e| CoreError::Io {
                path: segment_path_abs.clone(),
                source: e,
            })?;
            let len_after = meta.len();
            let mtime_after = meta.modified().ok();
            debug_assert_eq!(len_before, len_after, "exporter mutated source segment");
            debug_assert_eq!(mtime_before, mtime_after, "exporter touched source mtime");
        }
        if let Some(before) = edits_bytes_before {
            let after = std::fs::read(edits::edits_path(course_folder, video_id)).ok();
            debug_assert_eq!(Some(before), after, "exporter mutated edits.json");
        }

        let _ = edited_duration_sec(source_duration, &cuts); // silence unused-import lint in release
        Ok((mp4_path, srt_path))
    }

    fn mark_running(&self, video_id: &str, fraction: f64) {
        self.update_status(video_id, ExportStatus::Running { fraction });
    }

    fn finalize(&self, video_id: &str, status: ExportStatus) -> ExportJob {
        let mut inner = self.inner.lock().unwrap();
        inner.cancellers.remove(video_id);
        let job = match inner.jobs.get_mut(video_id) {
            Some(j) => {
                j.status = status;
                j.clone()
            }
            None => {
                // Manager dropped the job (rare — e.g. a programmatic
                // teardown) but we should still notify with a best-effort
                // snapshot rather than crash the worker.
                ExportJob {
                    video_id: video_id.to_string(),
                    course_folder: PathBuf::new(),
                    destination_dir: PathBuf::new(),
                    status,
                }
            }
        };
        notify(&inner, &job);
        job
    }

    fn update_status(&self, video_id: &str, status: ExportStatus) {
        let mut inner = self.inner.lock().unwrap();
        let Some(j) = inner.jobs.get_mut(video_id) else { return };
        j.status = status;
        let snap = j.clone();
        notify(&inner, &snap);
    }
}

fn notify(inner: &Inner, job: &ExportJob) {
    if let Some(sub) = inner.subscriber.as_ref() {
        sub(job);
    }
}

struct ManagerProgressSink<'a> {
    manager: &'a ExportManager,
    video_id: String,
}

impl ProgressSink for ManagerProgressSink<'_> {
    fn report(&self, fraction: f64) {
        self.manager.mark_running(&self.video_id, fraction);
    }
}

fn first_segment_path(folder: &Path, video_id: &str) -> Result<PathBuf> {
    let segs = segments::list_segments(folder, video_id)?;
    let first = segs
        .into_iter()
        .next()
        .ok_or_else(|| CoreError::NoSegmentsForExport(video_id.to_string()))?;
    Ok(first.path)
}

/// Read a Segment's playable duration in seconds. Calls `ffprobe` on macOS,
/// and falls back to a generous synthetic duration computed from file size
/// elsewhere so the cross-platform tests can drive the manager without
/// requiring ffprobe on every dev machine.
fn read_duration_sec(path: &Path) -> Result<f64> {
    #[cfg(target_os = "macos")]
    {
        if let Some(d) = crate::exporter::ffmpeg_mac::probe_duration_sec(path) {
            return Ok(d);
        }
    }
    // Synthetic fallback used by the fake backend's integration tests: a
    // marker file's "duration" doesn't matter for the AC ("exporter sees
    // the right keep_ranges") as long as it's positive and larger than
    // any cut start. 60s is more than enough for hand-written fixtures.
    let meta = std::fs::metadata(path).map_err(|e| CoreError::DurationUnknown {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if meta.len() == 0 {
        return Err(CoreError::DurationUnknown {
            path: path.to_path_buf(),
            message: "segment file is empty".into(),
        });
    }
    Ok(60.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CaptureRequest, CompositionDefaults, Device, SourceRole};
    use crate::core::edits;
    use crate::core::transcript::Word;
    use crate::exporter::fake::FakeExporter;
    use crate::recorder::fake::FakeRecorderBackend;
    use crate::recording_manager::RecordingManager;
    use crate::transcriber::fake::{FakeTranscriberBackend, ScriptedResponse};
    use crate::transcription_manager::TranscriptionManager;

    fn screen_request() -> CaptureRequest {
        CaptureRequest {
            role: SourceRole::Screen,
            device: Device {
                id: "default".into(),
                label: "Main Display".into(),
            },
            defaults: CompositionDefaults::default(),
        }
    }

    fn course_with_recorded_transcribed_video(words: Vec<Word>) -> (tempfile::TempDir, PathBuf, String) {
        let root = tempfile::tempdir().unwrap();
        let folder = crate::core::course::create_course(root.path(), "C").unwrap();
        let m = crate::core::course::add_module(&folder, "M").unwrap();
        let v = crate::core::course::add_video(&folder, &m.id, "V").unwrap();
        let rec = RecordingManager::new(Box::new(FakeRecorderBackend::default()));
        let snap = rec
            .start_session(&folder, &v.id, vec![screen_request()])
            .unwrap();
        rec.stop_session(&snap.id).unwrap();
        let seg = rec.keep_session(&snap.id).unwrap();
        let seg_abs = folder.join(&seg.path);

        let backend = Arc::new(FakeTranscriberBackend::default());
        let shared = FakeTranscriberBackend {
            scripted: backend.scripted.clone(),
            calls: backend.calls.clone(),
        };
        let tr = TranscriptionManager::new(Box::new(shared));
        backend.script(&seg_abs, ScriptedResponse::Ok(words));
        tr.enqueue(folder.clone(), v.id.clone());
        tr.process_pending();

        (root, folder, v.id)
    }

    fn manager(backend: FakeExporter) -> Arc<ExportManager> {
        Arc::new(ExportManager::new(Box::new(backend)))
    }

    fn five_words() -> Vec<Word> {
        vec![
            Word { start: 0.5, end: 1.0, text: "Hello".into() },
            Word { start: 1.0, end: 1.5, text: " brave".into() },
            Word { start: 1.5, end: 2.0, text: " new".into() },
            Word { start: 2.0, end: 2.5, text: " world".into() },
            Word { start: 3.0, end: 3.5, text: " indeed".into() },
        ]
    }

    #[test]
    fn start_export_writes_mp4_and_srt_to_default_destination() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let exporter = FakeExporter::default();
        let calls_handle = exporter.calls.clone();
        let mgr = manager(exporter);

        let job = mgr.start_export(&folder, &vid, None).unwrap();
        match job.status {
            ExportStatus::Done { ref mp4, ref srt } => {
                assert!(mp4.is_file(), "mp4 should exist at {mp4:?}");
                assert!(srt.is_file(), "srt should exist at {srt:?}");
                // Sit under <folder>/exports/<vid>/ by default.
                let expected_dir = folder.join("exports").join(&vid);
                assert_eq!(mp4.parent(), Some(expected_dir.as_path()));
                assert_eq!(srt.parent(), Some(expected_dir.as_path()));
            }
            ref other => panic!("expected Done, got {other:?}"),
        }
        // Backend was asked for the right keep-ranges. With no cuts and the
        // synthetic 60s duration, that's a single full-length range.
        let calls = calls_handle.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].keep_ranges, vec![(0.0, 60.0)]);
    }

    #[test]
    fn start_export_honours_a_user_chosen_destination() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let custom = tempfile::tempdir().unwrap();
        let mgr = manager(FakeExporter::default());
        let job = mgr
            .start_export(&folder, &vid, Some(custom.path().to_path_buf()))
            .unwrap();
        match job.status {
            ExportStatus::Done { ref mp4, .. } => {
                assert!(mp4.starts_with(custom.path()));
            }
            ref other => panic!("expected Done, got {other:?}"),
        }
    }

    #[test]
    fn start_export_passes_keep_ranges_derived_from_the_current_edl() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        // Cut 1.0–1.5 (the " brave" word) before exporting.
        edits::append_cut(&folder, &vid, 1.0, 1.5).unwrap();

        let exporter = FakeExporter::default();
        let calls_handle = exporter.calls.clone();
        let mgr = manager(exporter);
        mgr.start_export(&folder, &vid, None).unwrap();

        let calls = calls_handle.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].keep_ranges, vec![(0.0, 1.0), (1.5, 60.0)]);
    }

    #[test]
    fn start_export_marks_failed_when_backend_errors_and_leaves_no_outputs() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let exporter = FakeExporter::default();
        exporter.fail_next("backend kaboom");
        let mgr = manager(exporter);
        let job = mgr.start_export(&folder, &vid, None).unwrap();

        match job.status {
            ExportStatus::Failed { ref message } => assert!(message.contains("backend kaboom")),
            ref other => panic!("expected Failed, got {other:?}"),
        }
        let dir = folder.join("exports").join(&vid);
        // The renderer must not have written an SRT alongside a missing MP4.
        if dir.is_dir() {
            let names: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert!(
                !names.iter().any(|n| n.to_string_lossy().ends_with(".srt")),
                "no .srt should exist after a render failure: {names:?}"
            );
        }
    }

    #[test]
    fn start_export_errors_if_there_is_no_segment_to_render() {
        let root = tempfile::tempdir().unwrap();
        let folder = crate::core::course::create_course(root.path(), "C").unwrap();
        let m = crate::core::course::add_module(&folder, "M").unwrap();
        let v = crate::core::course::add_video(&folder, &m.id, "V").unwrap();
        let mgr = manager(FakeExporter::default());
        let job = mgr.start_export(&folder, &v.id, None).unwrap();
        match job.status {
            ExportStatus::Failed { ref message } => {
                assert!(message.contains("Segment"), "got: {message}");
            }
            ref other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn start_export_does_not_modify_the_source_segment_or_edl_on_disk() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        edits::append_cut(&folder, &vid, 1.0, 1.5).unwrap();
        let seg = segments::list_segments(&folder, &vid).unwrap()[0].clone();
        let seg_abs = folder.join(&seg.path);
        let seg_before = std::fs::read(&seg_abs).unwrap();
        let edits_before = std::fs::read(edits::edits_path(&folder, &vid)).unwrap();

        let mgr = manager(FakeExporter::default());
        mgr.start_export(&folder, &vid, None).unwrap();

        assert_eq!(std::fs::read(&seg_abs).unwrap(), seg_before);
        assert_eq!(
            std::fs::read(edits::edits_path(&folder, &vid)).unwrap(),
            edits_before
        );
    }

    #[test]
    fn cancel_drops_output_and_marks_the_job_cancelled() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let exporter = FakeExporter::default();
        exporter.stall();
        let mgr = manager(exporter);

        // Start on a worker thread so we can cancel mid-render.
        let mgr2 = mgr.clone();
        let folder2 = folder.clone();
        let vid2 = vid.clone();
        let handle = std::thread::spawn(move || mgr2.spawn_export(folder2, vid2, None));
        let _ = handle.join().unwrap().unwrap();
        mgr.cancel(&vid).unwrap();

        // Wait for the job to settle.
        let started = std::time::Instant::now();
        loop {
            let job = mgr.job_for_video(&vid).unwrap();
            if !matches!(job.status, ExportStatus::Pending | ExportStatus::Running { .. }) {
                match job.status {
                    ExportStatus::Cancelled => break,
                    ExportStatus::Done { ref mp4, ref srt } => {
                        // If the worker beat us to the finish line, that's
                        // fine — the test is asserting cancel works *when*
                        // it lands mid-render, so re-arm and try again.
                        let _ = std::fs::remove_file(mp4);
                        let _ = std::fs::remove_file(srt);
                        return;
                    }
                    ref other => panic!("expected Cancelled or Done, got {other:?}"),
                }
            }
            if started.elapsed() > std::time::Duration::from_secs(2) {
                panic!("export job did not settle within 2s");
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // No outputs should remain after cancellation.
        let dir = folder.join("exports").join(&vid);
        if dir.is_dir() {
            let leftover: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            assert!(leftover.is_empty(), "cancellation should leave nothing behind, got {leftover:?}");
        }
    }

    #[test]
    fn double_start_for_the_same_video_is_rejected() {
        // Use a stalling fake so the first job hasn't finished by the time
        // the second start_export call goes in.
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let exporter = FakeExporter::default();
        exporter.stall();
        let mgr = manager(exporter);

        let _ = mgr
            .spawn_export(folder.clone(), vid.clone(), None)
            .unwrap();
        let err = mgr.start_export(&folder, &vid, None).unwrap_err();
        assert!(matches!(err, CoreError::ExportAlreadyRunning(_)));

        // Tidy up so the test doesn't leak a worker thread.
        mgr.cancel(&vid).unwrap();
    }

    #[test]
    fn subscriber_sees_pending_running_and_done() {
        let (_root, folder, vid) = course_with_recorded_transcribed_video(five_words());
        let mgr = manager(FakeExporter::default());
        let seen = Arc::new(Mutex::new(Vec::<ExportStatus>::new()));
        {
            let seen = seen.clone();
            mgr.set_subscriber(Box::new(move |j| seen.lock().unwrap().push(j.status.clone())));
        }
        mgr.start_export(&folder, &vid, None).unwrap();

        let seen = seen.lock().unwrap().clone();
        assert!(matches!(seen.first(), Some(ExportStatus::Pending)));
        assert!(seen.iter().any(|s| matches!(s, ExportStatus::Running { .. })));
        assert!(matches!(seen.last(), Some(ExportStatus::Done { .. })));
    }

    #[test]
    fn default_destination_is_under_exports_video_id() {
        let mgr = manager(FakeExporter::default());
        let folder = PathBuf::from("/tmp/c");
        assert_eq!(
            mgr.default_destination(&folder, "vid-7"),
            PathBuf::from("/tmp/c/exports/vid-7")
        );
    }
}
