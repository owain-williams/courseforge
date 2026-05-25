//! Runtime registry that owns active `RecordingSession`s and the per-session
//! `ActiveRecording` backend handle. Lives in Tauri-managed state so the
//! commands layer can address sessions by id across IPC calls.
//!
//! The split between this module and `core::recording` is deliberate: the
//! pure state machine lives in `core::` and is exhaustively tested without
//! any IO; this module wraps it with thread-safety, the recorder backend,
//! and the on-disk side-effects (`finalize_segment`, `discard_partial`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::core::error::{CoreError, Result};
use crate::core::permissions::CaptureSources;
use crate::core::recording::{RecordingSession, SessionState};
use crate::core::segments;
use crate::recorder::{ActiveRecording, RecorderBackend};
use crate::remuxer::Remuxer;

/// Snapshot of a session safe to ship over IPC. Mirrors [`RecordingSession`]
/// (the field names line up so the frontend can deserialize the same shape).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSnapshot {
    pub id: String,
    #[serde(rename = "videoId")]
    pub video_id: String,
    #[serde(rename = "segmentId")]
    pub segment_id: String,
    #[serde(rename = "courseFolder")]
    pub course_folder: PathBuf,
    pub state: SessionState,
    pub sources: CaptureSources,
}

struct ActiveSession {
    session: RecordingSession,
    course_folder: PathBuf,
    /// `None` once `stop()` has run; we keep the row around until the user
    /// decides Keep / Discard so the UI can still address it by id.
    recording: Option<Box<dyn ActiveRecording>>,
}

impl ActiveSession {
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            id: self.session.id.clone(),
            video_id: self.session.video_id.clone(),
            segment_id: self.session.segment_id.clone(),
            course_folder: self.course_folder.clone(),
            state: self.session.state,
            sources: self.session.sources,
        }
    }
}

pub struct RecordingManager {
    backend: Box<dyn RecorderBackend>,
    remuxer: Box<dyn Remuxer>,
    sessions: Mutex<HashMap<String, ActiveSession>>,
}

impl RecordingManager {
    pub fn new(backend: Box<dyn RecorderBackend>, remuxer: Box<dyn Remuxer>) -> Self {
        Self {
            backend,
            remuxer,
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
        sources: CaptureSources,
    ) -> Result<SessionSnapshot> {
        let (segment_id, partial_path) = segments::prepare_segment_path(course_folder, video_id)?;
        let mut session = RecordingSession::new(
            video_id.to_string(),
            segment_id,
            partial_path.clone(),
            sources,
        );

        // Spin up the backend first; if it fails we leave nothing behind (the
        // empty segments/ dir is harmless) and the session never enters the
        // registry, so callers get a clean error.
        let recording = self.backend.start(partial_path, sources)?;
        session.start()?;

        let id = session.id.clone();
        let entry = ActiveSession {
            session,
            course_folder: course_folder.to_path_buf(),
            recording: Some(recording),
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
        // before driving the backend's stop — ffmpeg shutdown can take a
        // few seconds and we don't want every other IPC call to block on
        // it. The session row stays in the registry (with `recording: None`)
        // so the UI's id-based addressing still works.
        let recording = {
            let mut sessions = self.sessions.lock().unwrap();
            let entry = sessions
                .get_mut(id)
                .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
            entry.recording.take()
        };

        if let Some(rec) = recording {
            rec.stop()?;
        }

        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions
            .get_mut(id)
            .ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        entry.session.stop()?;
        Ok(entry.snapshot())
    }

    /// "Keep". Finalises the `.partial.mkv` (remuxing it to `<id>.mp4`),
    /// drops the session from the registry, and returns the new Segment
    /// record so callers can update UI.
    pub fn keep_session(&self, id: &str) -> Result<segments::Segment> {
        let mut sessions = self.sessions.lock().unwrap();
        let entry = sessions.get_mut(id).ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        entry.session.mark_persisted()?;
        let remuxer = &*self.remuxer;
        let seg = segments::finalize_segment(
            &entry.course_folder,
            &entry.session.video_id,
            &entry.session.segment_id,
            |src, dst| remuxer.remux_to_mp4(src, dst),
        )?;
        sessions.remove(id);
        Ok(seg)
    }

    /// Adopt a crash-recovered `.partial.mkv` as a finished Segment. Same
    /// remux path as `keep_session` — the only difference is that no live
    /// `RecordingSession` is involved, so we don't need to update session
    /// state.
    pub fn adopt_orphan(
        &self,
        course_folder: &Path,
        video_id: &str,
        segment_id: &str,
    ) -> Result<segments::Segment> {
        let remuxer = &*self.remuxer;
        segments::finalize_segment(course_folder, video_id, segment_id, |src, dst| {
            remuxer.remux_to_mp4(src, dst)
        })
    }

    /// "Discard". Tears the recorder down if it's still running, removes the
    /// `.partial.mkv`, drops the session from the registry.
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
        segments::discard_partial(
            &entry.course_folder,
            &entry.session.video_id,
            &entry.session.segment_id,
        )?;
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
        let entry = sessions.get_mut(id).ok_or_else(|| CoreError::SessionNotFound(id.to_string()))?;
        f(entry)?;
        Ok(entry.snapshot())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recorder::fake::FakeRecorderBackend;
    use crate::remuxer::fake::FakeRemuxer;

    fn course_with_video() -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let folder = crate::core::course::create_course(dir.path(), "C").unwrap();
        let m = crate::core::course::add_module(&folder, "M").unwrap();
        let v = crate::core::course::add_video(&folder, &m.id, "V").unwrap();
        // We hand back the *temp* dir so it stays alive; folder is dir/<slug>.
        // Tests use folder via the dir; rederive.
        let v_id = v.id;
        let _ = folder;
        (dir, v_id)
    }

    fn manager() -> (RecordingManager, FakeRecorderBackend) {
        let backend = FakeRecorderBackend::default();
        // We hand the manager its own backend, but keep a clone of the event
        // log on the side so tests can assert on it.
        let log_backend = FakeRecorderBackend { events: backend.shared_log() };
        let mgr = RecordingManager::new(
            Box::new(log_backend),
            Box::new(FakeRemuxer::default()),
        );
        (mgr, backend)
    }

    fn course_folder(dir: &tempfile::TempDir) -> PathBuf {
        // create_course slugifies "C" → "c".
        dir.path().join("c")
    }

    #[test]
    fn start_session_creates_partial_file_and_transitions_to_recording() {
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let snap = mgr
            .start_session(&course_folder(&dir), &vid, CaptureSources::default())
            .unwrap();

        assert_eq!(snap.state, SessionState::Recording);
        assert_eq!(snap.video_id, vid);
        assert!(snap.segment_id.len() > 8);

        // Backend was asked to start with our partial path.
        let events = backend.events.lock().unwrap();
        assert!(matches!(events.first(), Some(crate::recorder::fake::FakeEvent::Start(..))));

        // Partial file exists on disk.
        let expected = course_folder(&dir)
            .join("videos")
            .join(&vid)
            .join("segments")
            .join(format!("{}.partial.mkv", snap.segment_id));
        assert!(expected.is_file(), "expected partial at {expected:?}");
    }

    #[test]
    fn pause_then_resume_round_trips_state_and_drives_backend() {
        let (dir, vid) = course_with_video();
        let (mgr, backend) = manager();
        let snap = mgr.start_session(&course_folder(&dir), &vid, CaptureSources::default()).unwrap();

        let paused = mgr.pause_session(&snap.id).unwrap();
        assert_eq!(paused.state, SessionState::Paused);
        let resumed = mgr.resume_session(&snap.id).unwrap();
        assert_eq!(resumed.state, SessionState::Recording);

        let events: Vec<_> = backend.events.lock().unwrap().iter().map(|e| match e {
            crate::recorder::fake::FakeEvent::Start(..) => "start",
            crate::recorder::fake::FakeEvent::Pause => "pause",
            crate::recorder::fake::FakeEvent::Resume => "resume",
            crate::recorder::fake::FakeEvent::Stop => "stop",
        }).collect();
        assert_eq!(events, vec!["start", "pause", "resume"]);
    }

    #[test]
    fn stop_then_keep_finalises_segment_and_evicts_session() {
        let (dir, vid) = course_with_video();
        let (mgr, _backend) = manager();
        let snap = mgr.start_session(&course_folder(&dir), &vid, CaptureSources::default()).unwrap();
        let stopped = mgr.stop_session(&snap.id).unwrap();
        assert_eq!(stopped.state, SessionState::AwaitingDecision);

        let seg = mgr.keep_session(&snap.id).unwrap();
        assert_eq!(seg.video_id, vid);
        assert_eq!(seg.id, snap.segment_id);

        // Final .mp4 exists (remuxed from .partial.mkv), .partial.mkv is gone.
        let segs_dir = course_folder(&dir).join("videos").join(&vid).join("segments");
        assert!(segs_dir.join(format!("{}.mp4", seg.id)).is_file());
        assert!(!segs_dir.join(format!("{}.partial.mkv", seg.id)).exists());

        // Session is gone from the registry.
        assert!(mgr.list_sessions().is_empty());
    }

    #[test]
    fn stop_then_discard_removes_partial_and_evicts_session() {
        let (dir, vid) = course_with_video();
        let (mgr, _backend) = manager();
        let snap = mgr.start_session(&course_folder(&dir), &vid, CaptureSources::default()).unwrap();
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
        let snap = mgr.start_session(&course_folder(&dir), &vid, CaptureSources::default()).unwrap();
        mgr.discard_session(&snap.id).unwrap();

        let kinds: Vec<&str> = backend.events.lock().unwrap().iter().map(|e| match e {
            crate::recorder::fake::FakeEvent::Start(..) => "start",
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

        let snap = mgr.start_session(&course_folder(&dir), &vid, CaptureSources::default()).unwrap();
        assert!(mgr.has_active_sessions());

        mgr.stop_session(&snap.id).unwrap();
        // Still active — user hasn't decided yet.
        assert!(mgr.has_active_sessions());

        mgr.keep_session(&snap.id).unwrap();
        assert!(!mgr.has_active_sessions());
    }
}
