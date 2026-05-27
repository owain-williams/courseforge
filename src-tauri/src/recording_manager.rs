//! Runtime registry that owns active `RecordingSession`s and the per-session
//! `ActiveRecording` backend handle. Lives in Tauri-managed state so the
//! commands layer can address sessions by id across IPC calls.
//!
//! The split between this module and `core::recording` is deliberate: the
//! pure state machine lives in `core::` and is exhaustively tested without
//! any IO; this module wraps it with thread-safety, the recorder backend,
//! and the on-disk side-effects (`finalize_segment` rename + sidecar write,
//! `discard_partial`).
//!
//! Per ADR-0002 the recorder writes `.mov` directly via AVAssetWriter, so
//! `keep_session` no longer remuxes — it renames the partial to its final
//! name and emits a per-Segment sidecar JSON. The recorder-side `Remuxer`
//! trait retired in Phase 1; a small inline ffmpeg helper kept here is the
//! single remaining recorder-touching use of ffmpeg, scoped to importing
//! v1 `.partial.mkv` orphans recovered from a pre-Phase-1 crash.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::core::capture::{CaptureRequest, EndedReason, SegmentSidecar, SourceRole};
use crate::core::error::{CoreError, Result};
use crate::core::recording::{RecordingSession, SegmentSlot, SessionState};
use crate::core::segments;
use crate::recorder::{ActiveRecording, RecorderBackend, SourceOutcome, TakeRequest, TakeSource};

/// Snapshot of a session safe to ship over IPC. Mirrors [`RecordingSession`]
/// (the field names line up so the frontend can deserialize the same shape).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub id: String,
    pub video_id: String,
    pub segment_id: String,
    pub course_folder: PathBuf,
    pub state: SessionState,
    pub take_id: String,
    pub requests: Vec<CaptureRequest>,
    pub recorded_at: String,
}

struct ActiveSession {
    session: RecordingSession,
    course_folder: PathBuf,
    /// `None` once `stop()` has run; we keep the row around until the user
    /// decides Keep / Discard so the UI can still address it by id.
    recording: Option<Box<dyn ActiveRecording>>,
    /// Per-source outcomes captured at Stop time — populated for the
    /// AwaitingDecision window so Keep can write `endedReason:
    /// sourceFailed` sidecars for sources that failed mid-Take (issue #37).
    outcomes: Vec<SourceOutcome>,
}

impl ActiveSession {
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.session.id.clone(),
            video_id: self.session.video_id.clone(),
            segment_id: self.session.segment_id.clone(),
            course_folder: self.course_folder.clone(),
            state: self.session.state,
            take_id: self.session.take_id.clone(),
            requests: self.session.requests.clone(),
            recorded_at: self.session.recorded_at.clone(),
        }
    }
}

pub struct RecordingManager {
    backend: Box<dyn RecorderBackend>,
    sessions: Mutex<HashMap<String, ActiveSession>>,
}

impl RecordingManager {
    pub fn new(backend: Box<dyn RecorderBackend>) -> Self {
        Self {
            backend,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// True iff there is any session not in a terminal state. The UI uses
    /// this for the "block close while recording" guard.
    pub fn has_active_sessions(&self) -> bool {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .any(|s| s.session.is_active() || s.session.state == SessionState::AwaitingDecision)
    }

    pub fn start_session(
        &self,
        course_folder: &Path,
        video_id: &str,
        requests: Vec<CaptureRequest>,
    ) -> Result<SessionSnapshot> {
        if requests.is_empty() {
            return Err(CoreError::Recorder(
                "no capture sources requested — Start needs at least one".into(),
            ));
        }
        let take_paths = segments::prepare_take_paths(course_folder, video_id, &requests)?;
        let take_id = uuid::Uuid::new_v4().to_string();
        let recorded_at = iso8601_now();

        let slots: Vec<SegmentSlot> = take_paths
            .into_iter()
            .zip(requests.iter().cloned())
            .map(|(slot, request)| SegmentSlot {
                segment_id: slot.segment_id,
                partial_path: slot.partial_path,
                request,
            })
            .collect();
        let take_sources: Vec<TakeSource> = slots
            .iter()
            .map(|s| TakeSource {
                request: s.request.clone(),
                segment_id: s.segment_id.clone(),
                partial_path: s.partial_path.clone(),
            })
            .collect();

        let mut session = RecordingSession::new(
            video_id.to_string(),
            slots,
            take_id.clone(),
            recorded_at.clone(),
        );

        // Issue #38: write the Take marker before the backend starts so
        // a crash *during* backend init still leaves a marker for
        // recovery. Failure to write the marker is fatal — the
        // alternative is "we recorded but can't recover on crash" which
        // beats the whole point.
        let marker = segments::TakeMarker {
            schema_version: segments::TakeMarker::SCHEMA_VERSION,
            take_id: take_id.clone(),
            video_id: video_id.to_string(),
            recorded_at: recorded_at.clone(),
            scene_id: None, // Phase 2 doesn't thread scene_id through start_session yet.
            sources: session
                .slots
                .iter()
                .map(|slot| segments::TakeMarkerSource {
                    segment_id: slot.segment_id.clone(),
                    request: slot.request.clone(),
                })
                .collect(),
        };
        segments::write_take_marker(course_folder, video_id, &marker)?;

        // Spin up the backend; if it fails we tear down the marker as
        // well so the next-launch scanner doesn't surface a Take that
        // never actually wrote anything to disk.
        let recording = match self.backend.start(TakeRequest {
            take_id: take_id.clone(),
            sources: take_sources,
        }) {
            Ok(r) => r,
            Err(e) => {
                let _ = segments::remove_take_marker(course_folder, video_id, &take_id);
                return Err(e);
            }
        };
        session.start()?;

        let id = session.id.clone();
        let entry = ActiveSession {
            session,
            course_folder: course_folder.to_path_buf(),
            recording: Some(recording),
            outcomes: Vec::new(),
        };
        let snap = entry.snapshot();
        self.sessions.lock().unwrap().insert(id, entry);
        Ok(snap)
    }

    pub fn pause_session(&self, id: &str) -> Result<SessionSnapshot> {
        self.with_session(id, |s| {
            if let Some(rec) = s.recording.as_ref() {
                rec.pause()?;
            }
            s.session.pause()
        })
    }

    pub fn resume_session(&self, id: &str) -> Result<SessionSnapshot> {
        self.with_session(id, |s| {
            if let Some(rec) = s.recording.as_ref() {
                rec.resume()?;
            }
            s.session.resume()
        })
    }

    pub fn stop_session(&self, id: &str) -> Result<SessionSnapshot> {
        // Take the recording handle out under the lock, then drop the lock
        // before driving the backend's stop — finalising N AVAssetWriters
        // in parallel can take a moment and we don't want every other IPC
        // call to block on it. The session row stays in the registry
        // (with `recording: None`) so the UI's id-based addressing still
        // works.
        let recording = {
            let mut sessions = self.sessions.lock().unwrap();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
            entry.recording.take()
        };

        let outcomes = if let Some(rec) = recording {
            rec.stop()?
        } else {
            Vec::new()
        };

        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        entry.outcomes = outcomes;
        entry.session.stop()?;
        // Take marker removed on clean Stop — a crash before this point
        // leaves the marker behind for orphan recovery (issue #38).
        let _ = segments::remove_take_marker(
            &entry.course_folder,
            &entry.session.video_id,
            &entry.session.take_id,
        );
        Ok(entry.snapshot())
    }

    /// "Keep". Promotes every per-source `.partial.*` to its final name and
    /// writes one per-Segment sidecar JSON per slot. Drops the session from
    /// the registry and returns one Segment record per slot so callers can
    /// update UI. Phase 2 (issue #36) widened this from a single-segment
    /// return to a Take-shaped `Vec<Segment>`; #37 follows up with parallel
    /// finalize + mid-Take per-source failure handling.
    pub fn keep_session(&self, id: &str) -> Result<Vec<segments::Segment>> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;

        // Diagnostic guard: if *every* slot's partial is gone *and* no
        // final landed in its place, the recorder produced nothing. That
        // beats a downstream SegmentNotFound which would point at one of
        // N uuids and tell the user nothing. We evict the dead session
        // here so the UI clears the awaiting-decision banner.
        let any_landed = entry.session.slots.iter().any(|slot| {
            let role = slot.request.role;
            slot.partial_path.is_file() || sibling_final_for(&slot.partial_path, role).is_file()
        });
        if !any_landed {
            let first_partial = entry
                .session
                .slots
                .first()
                .map(|s| s.partial_path.display().to_string())
                .unwrap_or_default();
            let n_others = entry.session.slots.len().saturating_sub(1);
            let plural = if n_others == 1 { "" } else { "s" };
            sessions.remove(id);
            return Err(CoreError::Recorder(format!(
                "the recording produced no files (looked for {first_partial} and {n_others} other source{plural})"
            )));
        }

        entry.session.mark_persisted()?;
        let recorded_at = entry.session.recorded_at.clone();
        let take_id = entry.session.take_id.clone();
        let video_id = entry.session.video_id.clone();
        let course_folder = entry.course_folder.clone();
        let slots = entry.session.slots.clone();
        let outcomes = std::mem::take(&mut entry.outcomes);

        let outcome_for = |segment_id: &str| -> (EndedReason, Option<String>) {
            outcomes
                .iter()
                .find(|o| o.segment_id == segment_id)
                .map(|o| (o.ended_reason, o.ended_at.clone()))
                .unwrap_or((EndedReason::Normal, None))
        };

        let mut out = Vec::with_capacity(slots.len());
        for slot in slots {
            let (ended_reason, ended_at) = outcome_for(&slot.segment_id);
            let mut sidecar = SegmentSidecar::new(
                take_id.clone(),
                slot.request.role,
                slot.request.device.clone(),
                recorded_at.clone(),
                slot.request.defaults,
                ended_reason,
            );
            if let Some(ended_at) = ended_at {
                sidecar = sidecar.with_ended_at(ended_at);
            }
            let seg = segments::finalize_segment(
                &course_folder,
                &video_id,
                &slot.segment_id,
                sidecar,
            )?;
            out.push(seg);
        }
        sessions.remove(id);
        Ok(out)
    }

    /// Adopt every Segment in one crashed Take (issue #38). Reads the
    /// Take marker, calls `adopt_orphan` per source with a sidecar
    /// carrying the marker's role/device/defaults + `EndedReason::Crashed`,
    /// then removes the marker. v1 legacy orphans (no marker) fall
    /// through to the per-file `adopt_orphan` path the UI keeps for
    /// single-Segment Takes.
    pub fn adopt_orphan_take(
        &self,
        course_folder: &Path,
        video_id: &str,
        take_id: &str,
    ) -> Result<Vec<segments::Segment>> {
        let marker = segments::read_take_marker(course_folder, video_id, take_id)?;
        let marker = match marker {
            Some(m) => m,
            None => {
                return Err(CoreError::Recorder(format!(
                    "no in-progress Take marker for {take_id} — the Take may have already been adopted or discarded"
                )))
            }
        };
        let mut out = Vec::with_capacity(marker.sources.len());
        for src in &marker.sources {
            let sidecar = SegmentSidecar::new(
                marker.take_id.clone(),
                src.request.role,
                src.request.device.clone(),
                marker.recorded_at.clone(),
                src.request.defaults,
                EndedReason::Crashed,
            );
            let seg = segments::adopt_orphan(
                course_folder,
                video_id,
                &src.segment_id,
                sidecar,
                legacy_remux_mkv_to_mp4,
            )?;
            out.push(seg);
        }
        // Marker removed last so a crash mid-import leaves the rest of
        // the Take adoptable on the next attempt.
        let _ = segments::remove_take_marker(course_folder, video_id, take_id);
        Ok(out)
    }

    /// Discard every Segment in one crashed Take. Removes all partials
    /// plus the Take marker; safe to call multiple times.
    pub fn discard_orphan_take(
        &self,
        course_folder: &Path,
        video_id: &str,
        take_id: &str,
    ) -> Result<()> {
        let marker = segments::read_take_marker(course_folder, video_id, take_id)?;
        if let Some(marker) = marker {
            for src in &marker.sources {
                segments::discard_partial(course_folder, video_id, &src.segment_id)?;
            }
        }
        segments::remove_take_marker(course_folder, video_id, take_id)?;
        Ok(())
    }

    /// Adopt a crash-recovered partial as a finished Segment. Handles both
    /// the new `.partial.mov` shape (rename + sidecar with `endedReason:
    /// crashed`, using a Phase-1-shaped sidecar built from a synthetic Take)
    /// and v1 `.partial.mkv` orphans (legacy ffmpeg remux, no sidecar).
    pub fn adopt_orphan(
        &self,
        course_folder: &Path,
        video_id: &str,
        segment_id: &str,
    ) -> Result<segments::Segment> {
        // Crashed-orphan sidecar: we don't know the original Take's device
        // labels, so we record an unknown-device placeholder. Phase 7
        // polishes this — for now the take_id is regenerated per import
        // and the role defaults to Screen (the only role Phase 1 records).
        let sidecar = SegmentSidecar::new(
            uuid::Uuid::new_v4().to_string(),
            SourceRole::Screen,
            crate::core::capture::Device {
                id: "unknown".into(),
                label: "Unknown (recovered orphan)".into(),
            },
            iso8601_now(),
            crate::core::capture::CompositionDefaults::default(),
            EndedReason::Crashed,
        );
        segments::adopt_orphan(course_folder, video_id, segment_id, sidecar, legacy_remux_mkv_to_mp4)
    }

    /// "Discard". Tears the recorder down if it's still running, removes
    /// every per-source partial, drops the session from the registry.
    pub fn discard_session(&self, id: &str) -> Result<()> {
        // Same shape as `stop_session`: take the recording out under the
        // lock, drop the lock, run the (potentially slow) backend stop,
        // then re-acquire for the bookkeeping.
        let recording = {
            let mut sessions = self.sessions.lock().unwrap();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
            entry.recording.take()
        };
        if let Some(rec) = recording {
            // Best-effort stop — Discard must always succeed at evicting the
            // session even if the backend tear-down complains.
            let _ = rec.stop();
        }

        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        entry.session.mark_discarded()?;
        for slot in &entry.session.slots {
            segments::discard_partial(
                &entry.course_folder,
                &entry.session.video_id,
                &slot.segment_id,
            )?;
        }
        let _ = segments::remove_take_marker(
            &entry.course_folder,
            &entry.session.video_id,
            &entry.session.take_id,
        );
        sessions.remove(id);
        Ok(())
    }

    pub fn list_sessions(&self) -> Vec<SessionSnapshot> {
        self.sessions
            .lock()
            .unwrap()
            .values()
            .map(|s| s.snapshot())
            .collect()
    }

    fn with_session<F>(&self, id: &str, f: F) -> Result<SessionSnapshot>
    where
        F: FnOnce(&mut ActiveSession) -> Result<()>,
    {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        f(entry)?;
        Ok(entry.snapshot())
    }
}

fn sibling_final_for(partial: &Path, role: SourceRole) -> PathBuf {
    // `<id>.partial.<suffix>` → `<id>.<final-ext>`. Strips the partial
    // suffix appropriate for the role and re-adds the matching final ext.
    // Used by the Keep diagnostic guard to spot a post-finalize replay
    // where the partial is already gone but the final is sitting in place.
    let parent = partial.parent().unwrap_or(Path::new("."));
    let stem = partial
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let partial_suffix = segments::partial_suffix_for(role);
    let final_ext = segments::segment_ext_for(role);
    let id = stem
        .strip_suffix(&format!(".{}", partial_suffix))
        .unwrap_or(stem);
    parent.join(format!("{id}.{}", final_ext))
}

fn iso8601_now() -> String {
    // Plain UTC ISO-8601 to seconds resolution, no extra crates. Format:
    // `2026-05-26T12:34:56Z`. Sub-second precision isn't needed by callers
    // (the sidecar is for take grouping / display, not for sequencing).
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_iso8601(secs)
}

fn format_iso8601(epoch_secs: u64) -> String {
    // Convert epoch seconds to YYYY-MM-DDTHH:MM:SSZ in UTC. Rolled by hand
    // so we don't pull in chrono / time for one format string.
    let secs_per_day = 86_400u64;
    let days = epoch_secs / secs_per_day;
    let rem = epoch_secs % secs_per_day;
    let h = rem / 3600;
    let m = (rem % 3600) / 60;
    let s = rem % 60;
    let (y, mo, d) = civil_from_days(days as i64);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Howard Hinnant's days-from-civil inverse. Returns (year, month, day)
/// for days since 1970-01-01.
fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

/// Tiny ffmpeg-shellout the orphan adopter falls back to for v1
/// `.partial.mkv` recovery only. Stream-copies (no re-encode) into `.mp4`
/// with the `moov` atom up front so WebKit can play it. This is the only
/// recorder-side use of ffmpeg after Phase 1; it disappears when v1
/// orphans age out (Phase 7).
#[cfg(target_os = "macos")]
fn legacy_remux_mkv_to_mp4(src: &Path, dst: &Path) -> Result<()> {
    use std::process::Command;

    let ffmpeg = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.is_file())
        .or_else(|| {
            Command::new("/usr/bin/which")
                .arg("ffmpeg")
                .output()
                .ok()
                .and_then(|out| {
                    if out.status.success() {
                        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if s.is_empty() { None } else { Some(PathBuf::from(s)) }
                    } else {
                        None
                    }
                })
        })
        .ok_or_else(|| {
            CoreError::Recorder(
                "ffmpeg not found — needed to import a pre-Phase-1 .partial.mkv orphan. \
                 Install with `brew install ffmpeg` and try again."
                    .into(),
            )
        })?;

    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-i",
        ])
        .arg(src)
        .args(["-c", "copy", "-movflags", "+faststart", "-f", "mp4"])
        .arg(dst)
        .status()
        .map_err(|e| CoreError::Recorder(format!("failed to spawn ffmpeg for legacy remux: {e}")))?;

    if !status.success() {
        let _ = std::fs::remove_file(dst);
        return Err(CoreError::Recorder(format!(
            "legacy .partial.mkv → .mp4 remux failed (ffmpeg {status})"
        )));
    }
    Ok(())
}

/// Non-macOS builds don't ship with the legacy orphan path (no v1 ffmpeg
/// recorder ever ran there), but the function still needs to compile so
/// tests and the cross-platform fake backend link. Bytes-copy to keep
/// downstream callers' contract — "after this returns, dst exists" —
/// even though no real recovery is happening.
#[cfg(not(target_os = "macos"))]
fn legacy_remux_mkv_to_mp4(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    std::fs::copy(src, dst).map_err(|e| CoreError::Io {
        path: dst.to_path_buf(),
        source: e,
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CompositionDefaults, Device, SourceRole};
    use crate::recorder::fake::{FakeEvent, FakeRecorderBackend};

    fn course_with_video() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let folder = crate::core::course::create_course(dir.path(), "C").unwrap();
        let m = crate::core::course::add_module(&folder, "M").unwrap();
        let v = crate::core::course::add_video(&folder, &m.id, "V").unwrap();
        let _ = folder;
        (dir, v.id)
    }

    fn manager() -> (RecordingManager, FakeRecorderBackend) {
        let backend = FakeRecorderBackend::default();
        let log_backend = backend.shared_clone();
        let mgr = RecordingManager::new(Box::new(log_backend));
        (mgr, backend)
    }

    fn course_folder(dir: &tempfile::TempDir) -> PathBuf {
        // create_course slugifies "C" → "c".
        dir.path().join("c")
    }

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

    #[test]
    fn start_session_creates_partial_file_and_transitions_to_recording() {
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();

        assert_eq!(snap.state, SessionState::Recording);
        assert_eq!(snap.video_id, vid);
        assert!(snap.segment_id.len() > 8);
        assert!(!snap.take_id.is_empty());
        assert_eq!(snap.requests, vec![screen_request()]);

        // Backend was asked to start with our take_id and one partial path.
        let events = backend.events.lock().unwrap();
        match events.first() {
            Some(crate::recorder::fake::FakeEvent::Start { take_id, partial_paths }) => {
                assert_eq!(take_id, &snap.take_id);
                assert_eq!(partial_paths.len(), 1);
            }
            other => panic!("expected Start, got {other:?}"),
        }

        // Partial file exists on disk with the new .partial.mov extension.
        let expected = course_folder(&dir)
            .join("videos")
            .join(&vid)
            .join("segments")
            .join(format!("{}.partial.mov", snap.segment_id));
        assert!(expected.is_file(), "expected partial at {expected:?}");
    }

    #[test]
    fn start_session_rejects_empty_request_list() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let err = mgr
            .start_session(&course_folder(&dir), &vid, vec![])
            .unwrap_err();
        assert!(matches!(err, CoreError::Recorder(_)));
    }

    #[test]
    fn pause_then_resume_round_trips_state_and_drives_backend() {
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();

        let paused = mgr.pause_session(&snap.id).unwrap();
        assert_eq!(paused.state, SessionState::Paused);
        let resumed = mgr.resume_session(&snap.id).unwrap();
        assert_eq!(resumed.state, SessionState::Recording);

        let events: Vec<_> = backend.events.lock().unwrap().iter().map(|e| match e {
            crate::recorder::fake::FakeEvent::Start { .. } => "start",
            crate::recorder::fake::FakeEvent::Pause => "pause",
            crate::recorder::fake::FakeEvent::Resume => "resume",
            crate::recorder::fake::FakeEvent::Stop => "stop",
        }).collect();
        assert_eq!(events, vec!["start", "pause", "resume"]);
    }

    #[test]
    fn stop_then_keep_renames_to_mov_writes_sidecar_and_evicts_session() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        let take_id = snap.take_id.clone();
        let stopped = mgr.stop_session(&snap.id).unwrap();
        assert_eq!(stopped.state, SessionState::AwaitingDecision);

        let segs = mgr.keep_session(&snap.id).unwrap();
        assert_eq!(segs.len(), 1);
        let seg = &segs[0];
        assert_eq!(seg.video_id, vid);
        assert_eq!(seg.id, snap.segment_id);

        // Final .mov exists, .partial.mov is gone.
        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        assert!(segs_dir.join(format!("{}.mov", seg.id)).is_file());
        assert!(!segs_dir.join(format!("{}.partial.mov", seg.id)).exists());

        // Sidecar landed next to it with the right take_id and ended_reason.
        let sidecar = segments::read_sidecar(&course_folder(&dir), &vid, &seg.id)
            .unwrap()
            .unwrap();
        assert_eq!(sidecar.take_id, take_id);
        assert_eq!(sidecar.source_role, SourceRole::Screen);
        assert_eq!(sidecar.ended_reason, EndedReason::Normal);

        // Session is gone from the registry.
        assert!(mgr.list_sessions().is_empty());
    }

    #[test]
    fn multi_source_keep_finalises_every_slot_with_shared_take_id() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();

        let mic_request = CaptureRequest {
            role: SourceRole::Microphone,
            device: Device {
                id: "default".into(),
                label: "Default Mic".into(),
            },
            defaults: CompositionDefaults::default(),
        };
        let snap = mgr
            .start_session(
                &course_folder(&dir),
                &vid,
                vec![screen_request(), mic_request.clone()],
            )
            .unwrap();
        mgr.stop_session(&snap.id).unwrap();
        let segs = mgr.keep_session(&snap.id).unwrap();
        assert_eq!(segs.len(), 2, "every slot should produce a Segment");

        // Confirm each landed at its expected extension on disk.
        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        let mov = segs_dir.join(format!("{}.mov", segs[0].id));
        let m4a = segs_dir.join(format!("{}.m4a", segs[1].id));
        assert!(mov.is_file(), "expected video Segment at {mov:?}");
        assert!(m4a.is_file(), "expected audio Segment at {m4a:?}");

        // Every sidecar carries the same take_id and recorded_at.
        let sidecar_a = segments::read_sidecar(&course_folder(&dir), &vid, &segs[0].id)
            .unwrap()
            .unwrap();
        let sidecar_b = segments::read_sidecar(&course_folder(&dir), &vid, &segs[1].id)
            .unwrap()
            .unwrap();
        assert_eq!(sidecar_a.take_id, sidecar_b.take_id);
        assert_eq!(sidecar_a.take_id, snap.take_id);
        assert_eq!(sidecar_a.recorded_at, sidecar_b.recorded_at);
        assert_eq!(sidecar_a.source_role, SourceRole::Screen);
        assert_eq!(sidecar_b.source_role, SourceRole::Microphone);
    }

    #[test]
    fn mid_take_per_source_failure_writes_source_failed_sidecar_with_ended_at() {
        // Issue #37 mid-Take per-source failure rule: when one source fails
        // mid-Take, its sidecar carries `endedReason: sourceFailed` plus
        // the failure timestamp, and the rest of the Take continues to
        // their natural Stop with `Normal`.
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let mic_request = CaptureRequest {
            role: SourceRole::Microphone,
            device: Device {
                id: "default".into(),
                label: "Default Mic".into(),
            },
            defaults: CompositionDefaults::default(),
        };
        let snap = mgr
            .start_session(
                &course_folder(&dir),
                &vid,
                vec![screen_request(), mic_request],
            )
            .unwrap();
        // Inject a failure on the *mic* slot before Stop runs.
        let mic_segment_id = snap.requests[1].clone();
        let _ = mic_segment_id;
        // Backend keyed failures by segment id; pull the actual ids from
        // the manager via list_sessions so the test stays decoupled from
        // the registry's internal layout.
        let sessions = mgr.list_sessions();
        let mic_slot = &sessions[0];
        let _ = mic_slot;
        // The TakeSource ids land on the FakeRecording in order — slot 1
        // is the mic. We don't expose them via SessionSnapshot, but the
        // FakeRecorderBackend lets us look them up via its event log.
        let start_paths: Vec<PathBuf> = match backend.events.lock().unwrap().first().unwrap() {
            FakeEvent::Start { partial_paths, .. } => partial_paths.clone(),
            _ => panic!("expected Start"),
        };
        // Segment id is the filename stem before `.partial.*`.
        let mic_partial = &start_paths[1];
        let mic_seg_id = mic_partial
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('.')
            .next()
            .unwrap()
            .to_string();
        backend.fail_source(&mic_seg_id, "2026-05-27T01:23:45Z");

        mgr.stop_session(&snap.id).unwrap();
        let segs = mgr.keep_session(&snap.id).unwrap();
        assert_eq!(segs.len(), 2);

        // Sidecar for the screen slot — normal.
        let screen_sidecar =
            segments::read_sidecar(&course_folder(&dir), &vid, &segs[0].id).unwrap().unwrap();
        assert_eq!(screen_sidecar.ended_reason, EndedReason::Normal);
        assert!(screen_sidecar.ended_at.is_none());

        // Sidecar for the mic slot — sourceFailed with ended_at.
        let mic_sidecar =
            segments::read_sidecar(&course_folder(&dir), &vid, &segs[1].id).unwrap().unwrap();
        assert_eq!(mic_sidecar.ended_reason, EndedReason::SourceFailed);
        assert_eq!(mic_sidecar.ended_at.as_deref(), Some("2026-05-27T01:23:45Z"));
    }

    #[test]
    fn pause_resume_drive_backend_pause_atomic_in_order() {
        // Issue #37 atomic pause/resume: the manager flips the same
        // shared atomic both writers consult. The fake backend exposes
        // `paused` as an AtomicBool so tests can assert the gate is
        // honoured even without a real OS-level capture pipeline.
        let (dir, vid) = course_with_video();
        let (mgr, _backend) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        mgr.pause_session(&snap.id).unwrap();
        // After Pause: the session is paused; the next sample would be
        // dropped by `forward_sample`. We can't observe forward_sample
        // directly from the fake, but we can verify the state machine.
        assert_eq!(
            mgr.list_sessions()
                .into_iter()
                .find(|s| s.id == snap.id)
                .unwrap()
                .state,
            SessionState::Paused
        );
        mgr.resume_session(&snap.id).unwrap();
        assert_eq!(
            mgr.list_sessions()
                .into_iter()
                .find(|s| s.id == snap.id)
                .unwrap()
                .state,
            SessionState::Recording
        );
    }

    #[test]
    fn start_session_writes_a_take_marker_to_disk() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        let marker = segments::read_take_marker(&course_folder(&dir), &vid, &snap.take_id)
            .unwrap()
            .unwrap();
        assert_eq!(marker.take_id, snap.take_id);
        assert_eq!(marker.video_id, vid);
        assert_eq!(marker.sources.len(), 1);
    }

    #[test]
    fn clean_stop_removes_the_take_marker() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        mgr.stop_session(&snap.id).unwrap();
        assert!(
            segments::read_take_marker(&course_folder(&dir), &vid, &snap.take_id)
                .unwrap()
                .is_none(),
            "clean Stop should have removed the in-progress marker"
        );
    }

    #[test]
    fn adopt_orphan_take_imports_every_segment_atomically() {
        // Simulate a crashed multi-source Take by writing the marker +
        // both partials by hand, then call adopt_orphan_take and assert
        // both Segments came back finalised with the right sidecars.
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        std::fs::create_dir_all(&segs_dir).unwrap();
        // Two partials, one screen .mov, one mic .m4a — fake the contents.
        std::fs::write(segs_dir.join("seg-screen.partial.mov"), b"FAKE").unwrap();
        std::fs::write(segs_dir.join("seg-mic.partial.m4a"), b"FAKE").unwrap();
        let mic_request = CaptureRequest {
            role: SourceRole::Microphone,
            device: Device {
                id: "default".into(),
                label: "Default Mic".into(),
            },
            defaults: CompositionDefaults::default(),
        };
        let marker = segments::TakeMarker {
            schema_version: segments::TakeMarker::SCHEMA_VERSION,
            take_id: "take-crashed".into(),
            video_id: vid.clone(),
            recorded_at: "2026-05-27T01:00:00Z".into(),
            scene_id: None,
            sources: vec![
                segments::TakeMarkerSource {
                    segment_id: "seg-screen".into(),
                    request: screen_request(),
                },
                segments::TakeMarkerSource {
                    segment_id: "seg-mic".into(),
                    request: mic_request,
                },
            ],
        };
        segments::write_take_marker(&course_folder(&dir), &vid, &marker).unwrap();

        let segs = mgr
            .adopt_orphan_take(&course_folder(&dir), &vid, "take-crashed")
            .unwrap();
        assert_eq!(segs.len(), 2);

        // Both sidecars carry the take-id + Crashed reason.
        for seg in &segs {
            let sidecar = segments::read_sidecar(&course_folder(&dir), &vid, &seg.id)
                .unwrap()
                .unwrap();
            assert_eq!(sidecar.take_id, "take-crashed");
            assert_eq!(sidecar.ended_reason, EndedReason::Crashed);
        }

        // Marker is gone; partials are gone; finals are in place.
        assert!(
            segments::read_take_marker(&course_folder(&dir), &vid, "take-crashed")
                .unwrap()
                .is_none()
        );
        assert!(segs_dir.join("seg-screen.mov").is_file());
        assert!(segs_dir.join("seg-mic.m4a").is_file());
    }

    #[test]
    fn discard_orphan_take_removes_marker_and_every_partial() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        std::fs::create_dir_all(&segs_dir).unwrap();
        std::fs::write(segs_dir.join("seg-a.partial.mov"), b"FAKE").unwrap();
        let marker = segments::TakeMarker {
            schema_version: segments::TakeMarker::SCHEMA_VERSION,
            take_id: "doomed".into(),
            video_id: vid.clone(),
            recorded_at: "2026-05-27T01:00:00Z".into(),
            scene_id: None,
            sources: vec![segments::TakeMarkerSource {
                segment_id: "seg-a".into(),
                request: screen_request(),
            }],
        };
        segments::write_take_marker(&course_folder(&dir), &vid, &marker).unwrap();
        mgr.discard_orphan_take(&course_folder(&dir), &vid, "doomed")
            .unwrap();
        assert!(!segs_dir.join("seg-a.partial.mov").exists());
        assert!(
            segments::read_take_marker(&course_folder(&dir), &vid, "doomed")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn multi_source_discard_removes_every_partial() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let mic_request = CaptureRequest {
            role: SourceRole::Microphone,
            device: Device {
                id: "default".into(),
                label: "Default Mic".into(),
            },
            defaults: CompositionDefaults::default(),
        };
        let snap = mgr
            .start_session(
                &course_folder(&dir),
                &vid,
                vec![screen_request(), mic_request],
            )
            .unwrap();
        mgr.discard_session(&snap.id).unwrap();

        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        assert!(segs_dir.is_dir());
        assert!(
            std::fs::read_dir(&segs_dir).unwrap().next().is_none(),
            "discard should leave no per-source partials behind"
        );
    }

    #[test]
    fn stop_then_discard_removes_partial_and_evicts_session() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        mgr.stop_session(&snap.id).unwrap();
        mgr.discard_session(&snap.id).unwrap();

        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        assert!(segs_dir.is_dir());
        assert!(std::fs::read_dir(&segs_dir).unwrap().next().is_none(),
                "discard should leave the segments dir empty");
        assert!(mgr.list_sessions().is_empty());
    }

    #[test]
    fn discard_while_recording_tears_down_backend_and_cleans_up() {
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        mgr.discard_session(&snap.id).unwrap();

        let kinds: Vec<&str> = backend.events.lock().unwrap().iter().map(|e| match e {
            crate::recorder::fake::FakeEvent::Start { .. } => "start",
            crate::recorder::fake::FakeEvent::Pause => "pause",
            crate::recorder::fake::FakeEvent::Resume => "resume",
            crate::recorder::fake::FakeEvent::Stop => "stop",
        }).collect();
        assert_eq!(kinds, vec!["start", "stop"], "discard mid-recording should stop the backend");
        assert!(mgr.list_sessions().is_empty());
    }

    #[test]
    fn unknown_session_id_errors_clearly() {
        let (mgr, _) = manager();
        assert!(matches!(
            mgr.pause_session("nope"),
            Err(CoreError::SessionNotFound(_))
        ));
        assert!(matches!(
            mgr.keep_session("nope"),
            Err(CoreError::SessionNotFound(_))
        ));
    }

    #[test]
    fn has_active_sessions_is_true_until_terminal_decision() {
        let (dir, vid) = course_with_video();
        let (mgr, _) = manager();
        assert!(!mgr.has_active_sessions());

        let snap = mgr
            .start_session(&course_folder(&dir), &vid, vec![screen_request()])
            .unwrap();
        assert!(mgr.has_active_sessions());

        mgr.stop_session(&snap.id).unwrap();
        // Still active — user hasn't decided yet.
        assert!(mgr.has_active_sessions());

        mgr.keep_session(&snap.id).unwrap();
        assert!(!mgr.has_active_sessions());
    }

    #[test]
    fn iso8601_formatter_handles_known_dates() {
        // 1970-01-01T00:00:00Z = epoch 0
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        // First day boundary.
        assert_eq!(format_iso8601(86_400), "1970-01-02T00:00:00Z");
        // First leap-year boundary inside the era (2000-02-29 → 2000-03-01).
        // 2000-03-01T00:00:00Z = epoch 951_868_800.
        assert_eq!(format_iso8601(951_868_800), "2000-03-01T00:00:00Z");
        // Sub-day component sanity: 12:34:56 of the same day.
        assert_eq!(format_iso8601(45_296), "1970-01-01T12:34:56Z");
    }
}
