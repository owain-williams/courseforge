//! In-memory recorder used by the session-manager tests and as the fallback
//! backend on non-macOS dev machines.
//!
//! Behaviour: `start` touches the `.partial.mkv` so downstream code sees a
//! real file, and `pause` / `resume` / `stop` just update an `Arc<Mutex>`
//! transition log. Tests can introspect that log to verify the manager
//! called the backend in the right order.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::error::{CoreError, Result};
use crate::core::permissions::CaptureSources;
use super::{ActiveRecording, RecorderBackend};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeEvent {
    Start(PathBuf, CaptureSources),
    Pause,
    Resume,
    Stop,
}

#[derive(Default)]
pub struct FakeRecorderBackend {
    pub events: Arc<Mutex<Vec<FakeEvent>>>,
}

impl FakeRecorderBackend {
    pub fn shared_log(&self) -> Arc<Mutex<Vec<FakeEvent>>> {
        self.events.clone()
    }
}

impl RecorderBackend for FakeRecorderBackend {
    fn start(
        &self,
        partial_path: PathBuf,
        sources: CaptureSources,
    ) -> Result<Box<dyn ActiveRecording>> {
        // Touch the file so segments::finalize_segment sees something to rename.
        if let Some(parent) = partial_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::write(&partial_path, b"FAKE-PARTIAL-MKV").map_err(|e| CoreError::Io {
            path: partial_path.clone(),
            source: e,
        })?;

        self.events
            .lock()
            .unwrap()
            .push(FakeEvent::Start(partial_path.clone(), sources));

        Ok(Box::new(FakeRecording {
            events: self.events.clone(),
            partial_path,
        }))
    }
}

pub struct FakeRecording {
    events: Arc<Mutex<Vec<FakeEvent>>>,
    #[allow(dead_code)] // held for parity with the real backend
    partial_path: PathBuf,
}

impl ActiveRecording for FakeRecording {
    fn pause(&self) -> Result<()> {
        self.events.lock().unwrap().push(FakeEvent::Pause);
        Ok(())
    }
    fn resume(&self) -> Result<()> {
        self.events.lock().unwrap().push(FakeEvent::Resume);
        Ok(())
    }
    fn stop(&self) -> Result<()> {
        self.events.lock().unwrap().push(FakeEvent::Stop);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_writes_a_partial_file_and_records_event() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("seg.partial.mkv");
        let backend = FakeRecorderBackend::default();
        let _h = backend.start(target.clone(), CaptureSources::default()).unwrap();

        assert!(target.is_file());
        let events = backend.events.lock().unwrap();
        assert!(matches!(events.first(), Some(FakeEvent::Start(p, _)) if p == &target));
    }

    #[test]
    fn pause_resume_stop_are_logged_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("seg.partial.mkv");
        let backend = FakeRecorderBackend::default();
        let h = backend.start(target, CaptureSources::default()).unwrap();
        h.pause().unwrap();
        h.resume().unwrap();
        h.stop().unwrap();

        let events = backend.events.lock().unwrap().clone();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                FakeEvent::Start(..) => "start",
                FakeEvent::Pause => "pause",
                FakeEvent::Resume => "resume",
                FakeEvent::Stop => "stop",
            })
            .collect();
        assert_eq!(kinds, vec!["start", "pause", "resume", "stop"]);
    }
}
