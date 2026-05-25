//! Serial transcription queue. Owns a single background worker that drains
//! pending jobs through the configured `TranscriberBackend`, persists the
//! resulting `transcript.json`, and surfaces per-Video status to the UI.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::core::error::{CoreError, Result};
use crate::core::segments;
use crate::core::transcript::{self, Transcript};
use crate::transcriber::{ProgressSink, TranscriberBackend};

/// Where a job is in its lifecycle. Sent to the frontend as part of the
/// per-Video snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum JobStatus {
    Pending,
    Running { fraction: f64 },
    Done,
    Failed { message: String },
}

/// Per-Video snapshot of the transcription job for that Video. Mirrors what
/// the UI binds to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionJob {
    #[serde(rename = "videoId")]
    pub video_id: String,
    #[serde(rename = "courseFolder")]
    pub course_folder: PathBuf,
    pub status: JobStatus,
}

/// A subscriber gets notified after every job state change (pending,
/// progress tick, terminal). Used to plumb status into Tauri events without
/// the manager itself depending on Tauri.
pub type Subscriber = Box<dyn Fn(&TranscriptionJob) + Send + Sync>;

pub struct TranscriptionManager {
    backend: Arc<dyn TranscriberBackend>,
    inner: Mutex<Inner>,
}

struct Inner {
    jobs: HashMap<String, TranscriptionJob>,
    queue: VecDeque<String>,
    subscriber: Option<Subscriber>,
}

impl TranscriptionManager {
    pub fn new(backend: Box<dyn TranscriberBackend>) -> Self {
        Self {
            backend: Arc::from(backend),
            inner: Mutex::new(Inner {
                jobs: HashMap::new(),
                queue: VecDeque::new(),
                subscriber: None,
            }),
        }
    }

    /// Install a one-shot subscriber. Replacing it is allowed; tests usually
    /// don't bother, while `lib::run` installs the Tauri event emitter.
    pub fn set_subscriber(&self, sub: Subscriber) {
        self.inner.lock().unwrap().subscriber = Some(sub);
    }

    /// Add (or re-add) a Video to the queue. Idempotent: calling enqueue for
    /// a Video that is already Pending or Running is a no-op, so the UI's
    /// "retry" button doesn't double-queue.
    pub fn enqueue(&self, course_folder: PathBuf, video_id: String) {
        let mut inner = self.inner.lock().unwrap();
        let existing = inner.jobs.get(&video_id).cloned();
        match existing {
            Some(j) if matches!(j.status, JobStatus::Pending | JobStatus::Running { .. }) => {
                // Already in flight — drop the duplicate enqueue.
                return;
            }
            _ => {}
        }
        let job = TranscriptionJob {
            video_id: video_id.clone(),
            course_folder,
            status: JobStatus::Pending,
        };
        inner.jobs.insert(video_id.clone(), job.clone());
        inner.queue.push_back(video_id);
        notify(&inner, &job);
    }

    /// Re-queue a Failed job. Errors if there's nothing to retry. A Done job
    /// can be re-queued too (the user might want to regenerate the
    /// transcript after a re-take).
    pub fn retry(&self, video_id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let job = inner
            .jobs
            .get(video_id)
            .ok_or_else(|| CoreError::TranscriptionJobNotFound(video_id.to_string()))?;
        if matches!(job.status, JobStatus::Pending | JobStatus::Running { .. }) {
            return Ok(());
        }
        let next = TranscriptionJob {
            video_id: video_id.to_string(),
            course_folder: job.course_folder.clone(),
            status: JobStatus::Pending,
        };
        inner.jobs.insert(video_id.to_string(), next.clone());
        inner.queue.push_back(video_id.to_string());
        notify(&inner, &next);
        Ok(())
    }

    /// Snapshot of all jobs, in insertion order. The UI iterates this to
    /// render progress indicators.
    pub fn jobs(&self) -> Vec<TranscriptionJob> {
        self.inner.lock().unwrap().jobs.values().cloned().collect()
    }

    pub fn job_for_video(&self, video_id: &str) -> Option<TranscriptionJob> {
        self.inner.lock().unwrap().jobs.get(video_id).cloned()
    }

    /// Drain every pending job synchronously. Production code calls this
    /// from a dedicated worker thread; tests call it inline.
    ///
    /// Each job:
    /// 1. Pops from the queue.
    /// 2. Transitions Pending → Running.
    /// 3. Asks the backend for words (using the first Segment on disk).
    /// 4. On success: writes `transcript.json`, marks Done.
    /// 5. On failure: marks Failed with the message; the rest of the queue
    ///    still drains.
    pub fn process_pending(&self) -> usize {
        let mut processed = 0;
        loop {
            let next_id = self.inner.lock().unwrap().queue.pop_front();
            let Some(video_id) = next_id else { break };
            self.process_one(&video_id);
            processed += 1;
        }
        processed
    }

    fn process_one(&self, video_id: &str) {
        let course_folder = match self.inner.lock().unwrap().jobs.get(video_id) {
            Some(j) => j.course_folder.clone(),
            None => return,
        };

        self.mark(video_id, JobStatus::Running { fraction: 0.0 });

        let segment_path = match find_segment_to_transcribe(&course_folder, video_id) {
            Ok(p) => p,
            Err(e) => {
                self.mark(video_id, JobStatus::Failed { message: e.to_string() });
                return;
            }
        };
        let segment_id = segment_id_from_path(&segment_path);

        let sink = ManagerProgressSink {
            manager: self,
            video_id: video_id.to_string(),
        };
        let backend = self.backend.clone();
        let words = backend.transcribe(&segment_path, &sink);

        match words {
            Ok(words) => {
                let transcript = Transcript::new(
                    video_id.to_string(),
                    segment_id.into_iter().collect(),
                    words,
                );
                if let Err(e) = transcript::write_transcript(&course_folder, video_id, &transcript) {
                    self.mark(video_id, JobStatus::Failed { message: e.to_string() });
                    return;
                }
                self.mark(video_id, JobStatus::Done);
            }
            Err(e) => {
                self.mark(video_id, JobStatus::Failed { message: e.to_string() });
            }
        }
    }

    fn mark(&self, video_id: &str, status: JobStatus) {
        let inner = self.inner.lock().unwrap();
        let Some(existing) = inner.jobs.get(video_id) else { return };
        let updated = TranscriptionJob {
            video_id: existing.video_id.clone(),
            course_folder: existing.course_folder.clone(),
            status,
        };
        drop(inner);
        let mut inner = self.inner.lock().unwrap();
        inner.jobs.insert(video_id.to_string(), updated.clone());
        notify(&inner, &updated);
    }
}

fn notify(inner: &Inner, job: &TranscriptionJob) {
    if let Some(sub) = inner.subscriber.as_ref() {
        sub(job);
    }
}

struct ManagerProgressSink<'a> {
    manager: &'a TranscriptionManager,
    video_id: String,
}

impl ProgressSink for ManagerProgressSink<'_> {
    fn report(&self, fraction: f64) {
        self.manager
            .mark(&self.video_id, JobStatus::Running { fraction });
    }
}

fn find_segment_to_transcribe(folder: &Path, video_id: &str) -> Result<PathBuf> {
    let segs = segments::list_segments(folder, video_id)?;
    let first = segs
        .into_iter()
        .next()
        .ok_or_else(|| CoreError::NoSegmentsForTranscription(video_id.to_string()))?;
    Ok(folder.join(first.path))
}

fn segment_id_from_path(p: &Path) -> Option<String> {
    p.file_stem().and_then(|s| s.to_str()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::transcript::Word;
    use crate::recorder::fake::FakeRecorderBackend;
    use crate::recording_manager::RecordingManager;
    use crate::transcriber::fake::{FakeTranscriberBackend, ScriptedResponse};

    fn make_manager() -> (TranscriptionManager, Arc<FakeTranscriberBackend>) {
        let backend = Arc::new(FakeTranscriberBackend::default());
        // Hand the manager a backend that shares state with the one we keep.
        let shared = FakeTranscriberBackend {
            scripted: backend.scripted.clone(),
            calls: backend.calls.clone(),
        };
        (TranscriptionManager::new(Box::new(shared)), backend)
    }

    fn course_with_recorded_segment() -> (tempfile::TempDir, PathBuf, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let folder = crate::core::course::create_course(dir.path(), "C").unwrap();
        let m = crate::core::course::add_module(&folder, "M").unwrap();
        let v = crate::core::course::add_video(&folder, &m.id, "V").unwrap();

        // Drive a real (fake-backed) recording through to a finalised Segment
        // so the transcription queue has a real Segment to find on disk.
        let rec_mgr = RecordingManager::new(Box::new(FakeRecorderBackend::default()));
        let snap = rec_mgr
            .start_session(&folder, &v.id, Default::default())
            .unwrap();
        rec_mgr.stop_session(&snap.id).unwrap();
        let seg = rec_mgr.keep_session(&snap.id).unwrap();

        (dir, folder, v.id, seg.id)
    }

    #[test]
    fn enqueue_adds_a_pending_job_visible_in_jobs_list() {
        let (mgr, _) = make_manager();
        mgr.enqueue(PathBuf::from("/tmp/whatever"), "vid-1".into());

        let jobs = mgr.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].video_id, "vid-1");
        assert!(matches!(jobs[0].status, JobStatus::Pending));
    }

    #[test]
    fn enqueue_is_idempotent_while_a_job_is_in_flight() {
        let (mgr, _) = make_manager();
        mgr.enqueue(PathBuf::from("/tmp/x"), "vid-1".into());
        mgr.enqueue(PathBuf::from("/tmp/x"), "vid-1".into());
        mgr.enqueue(PathBuf::from("/tmp/x"), "vid-1".into());

        let jobs = mgr.jobs();
        assert_eq!(jobs.len(), 1);
    }

    #[test]
    fn process_pending_runs_backend_and_writes_transcript_json() {
        let (_dir, folder, vid, _segid) = course_with_recorded_segment();
        let (mgr, _backend) = make_manager();

        mgr.enqueue(folder.clone(), vid.clone());
        let processed = mgr.process_pending();
        assert_eq!(processed, 1);

        let t = transcript::read_transcript(&folder, &vid).unwrap();
        let t = t.expect("transcript.json should have been written");
        assert_eq!(t.video_id, vid);
        assert!(!t.words.is_empty(), "should write at least one word");
    }

    #[test]
    fn process_pending_marks_done_on_success() {
        let (_dir, folder, vid, _) = course_with_recorded_segment();
        let (mgr, _backend) = make_manager();
        mgr.enqueue(folder, vid.clone());
        mgr.process_pending();

        let job = mgr.job_for_video(&vid).expect("job should exist");
        assert!(matches!(job.status, JobStatus::Done), "got {:?}", job.status);
    }

    #[test]
    fn process_pending_marks_failed_when_backend_errors_and_does_not_write_transcript() {
        let (_dir, folder, vid, _segid) = course_with_recorded_segment();
        let (mgr, backend) = make_manager();
        // Script the failure against the actual on-disk Segment path.
        let segs_dir = folder.join("videos").join(&vid).join("segments");
        let seg_path = std::fs::read_dir(&segs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|s| s.to_str()) == Some("mkv"))
            .expect("a finalised .mkv should exist");
        backend.script(&seg_path, ScriptedResponse::Err("ASR exploded".into()));

        mgr.enqueue(folder.clone(), vid.clone());
        mgr.process_pending();

        let job = mgr.job_for_video(&vid).unwrap();
        match job.status {
            JobStatus::Failed { message } => assert!(message.contains("ASR exploded")),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(transcript::read_transcript(&folder, &vid).unwrap().is_none());
    }

    #[test]
    fn retry_requeues_a_failed_job() {
        let (_dir, folder, vid, _) = course_with_recorded_segment();
        let (mgr, backend) = make_manager();
        let segs_dir = folder.join("videos").join(&vid).join("segments");
        let seg_path = std::fs::read_dir(&segs_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.extension().and_then(|s| s.to_str()) == Some("mkv"))
            .unwrap();
        backend.script(&seg_path, ScriptedResponse::Err("nope".into()));

        mgr.enqueue(folder.clone(), vid.clone());
        mgr.process_pending();
        assert!(matches!(mgr.job_for_video(&vid).unwrap().status, JobStatus::Failed { .. }));

        // Now make the next attempt succeed and retry.
        backend.script(
            &seg_path,
            ScriptedResponse::Ok(vec![Word { start: 0.0, end: 0.4, text: "ok".into() }]),
        );
        mgr.retry(&vid).unwrap();
        mgr.process_pending();

        assert!(matches!(mgr.job_for_video(&vid).unwrap().status, JobStatus::Done));
        assert_eq!(
            transcript::read_transcript(&folder, &vid).unwrap().unwrap().words[0].text,
            "ok"
        );
    }

    #[test]
    fn retry_on_unknown_video_errors_clearly() {
        let (mgr, _) = make_manager();
        let err = mgr.retry("ghost").unwrap_err();
        assert!(matches!(err, CoreError::TranscriptionJobNotFound(_)));
    }

    #[test]
    fn process_pending_continues_after_a_failure() {
        // Two jobs; the first errors (no segments on disk for that video),
        // the second succeeds. The queue must drain regardless.
        let (_dir, folder, vid_ok, _) = course_with_recorded_segment();
        let (mgr, _backend) = make_manager();

        mgr.enqueue(folder.clone(), "vid-no-segments".into());
        mgr.enqueue(folder.clone(), vid_ok.clone());
        let processed = mgr.process_pending();
        assert_eq!(processed, 2);

        assert!(matches!(
            mgr.job_for_video("vid-no-segments").unwrap().status,
            JobStatus::Failed { .. }
        ));
        assert!(matches!(
            mgr.job_for_video(&vid_ok).unwrap().status,
            JobStatus::Done
        ));
    }

    #[test]
    fn subscriber_sees_pending_running_and_done() {
        let (_dir, folder, vid, _) = course_with_recorded_segment();
        let (mgr, _backend) = make_manager();
        let seen = Arc::new(Mutex::new(Vec::<JobStatus>::new()));
        {
            let seen = seen.clone();
            mgr.set_subscriber(Box::new(move |j| {
                seen.lock().unwrap().push(j.status.clone());
            }));
        }
        mgr.enqueue(folder, vid);
        mgr.process_pending();

        let seen = seen.lock().unwrap().clone();
        // Expect at minimum: Pending → Running(0.0) → Running(1.0) → Done.
        assert!(matches!(seen.first(), Some(JobStatus::Pending)));
        assert!(matches!(seen.last(), Some(JobStatus::Done)));
        assert!(seen.iter().any(|s| matches!(s, JobStatus::Running { .. })));
    }
}
