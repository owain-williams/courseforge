//! In-memory recorder used by the session-manager tests and as the fallback
//! backend on non-macOS dev machines.
//!
//! Behaviour: `start` touches each source's `.partial.*` file so downstream
//! code sees a real file per slot, and `pause` / `resume` / `stop` just
//! update an `Arc<Mutex>` transition log. Tests can introspect that log to
//! verify the manager called the backend in the right order with the right
//! `TakeRequest`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::error::{CoreError, Result};
use super::{ActiveRecording, RecorderBackend, TakeRequest};

#[derive(Debug, Clone, PartialEq)]
pub enum FakeEvent {
    Start {
        take_id: String,
        partial_paths: Vec<PathBuf>,
    },
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
    fn start(&self, take: TakeRequest) -> Result<Box<dyn ActiveRecording>> {
        let mut paths = Vec::with_capacity(take.sources.len());
        for src in &take.sources {
            if let Some(parent) = src.partial_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
            std::fs::write(&src.partial_path, b"FAKE-PARTIAL").map_err(|e| CoreError::Io {
                path: src.partial_path.clone(),
                source: e,
            })?;
            paths.push(src.partial_path.clone());
        }

        self.events.lock().unwrap().push(FakeEvent::Start {
            take_id: take.take_id,
            partial_paths: paths.clone(),
        });

        Ok(Box::new(FakeRecording {
            events: self.events.clone(),
            _partial_paths: paths,
        }))
    }
}

pub struct FakeRecording {
    events: Arc<Mutex<Vec<FakeEvent>>>,
    _partial_paths: Vec<PathBuf>,
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
    use crate::core::capture::{CaptureRequest, CompositionDefaults, Device, SourceRole};
    use crate::recorder::TakeSource;

    fn req(role: SourceRole) -> CaptureRequest {
        CaptureRequest {
            role,
            device: Device {
                id: "default".into(),
                label: "Default".into(),
            },
            defaults: CompositionDefaults::default(),
        }
    }

    fn take_for(
        dir: &std::path::Path,
        sources: impl IntoIterator<Item = (CaptureRequest, &'static str)>,
    ) -> TakeRequest {
        let sources: Vec<TakeSource> = sources
            .into_iter()
            .enumerate()
            .map(|(i, (request, ext))| TakeSource {
                request,
                segment_id: format!("seg-{i}"),
                partial_path: dir.join(format!("seg-{i}.partial.{ext}")),
            })
            .collect();
        TakeRequest {
            take_id: "take-xyz".into(),
            sources,
        }
    }

    #[test]
    fn start_writes_a_partial_file_per_source_and_records_take_id() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FakeRecorderBackend::default();
        let take = take_for(
            dir.path(),
            [(req(SourceRole::Screen), "mov"), (req(SourceRole::Microphone), "m4a")],
        );
        let expected_paths = take
            .sources
            .iter()
            .map(|s| s.partial_path.clone())
            .collect::<Vec<_>>();
        let _h = backend.start(take).unwrap();

        for p in &expected_paths {
            assert!(p.is_file(), "expected partial at {}", p.display());
        }
        let events = backend.events.lock().unwrap();
        match events.first() {
            Some(FakeEvent::Start { take_id, partial_paths }) => {
                assert_eq!(take_id, "take-xyz");
                assert_eq!(partial_paths, &expected_paths);
            }
            other => panic!("expected Start event, got {other:?}"),
        }
    }

    #[test]
    fn pause_resume_stop_are_logged_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FakeRecorderBackend::default();
        let take = take_for(dir.path(), [(req(SourceRole::Screen), "mov")]);
        let h = backend.start(take).unwrap();
        h.pause().unwrap();
        h.resume().unwrap();
        h.stop().unwrap();

        let events = backend.events.lock().unwrap().clone();
        let kinds: Vec<&str> = events
            .iter()
            .map(|e| match e {
                FakeEvent::Start { .. } => "start",
                FakeEvent::Pause => "pause",
                FakeEvent::Resume => "resume",
                FakeEvent::Stop => "stop",
            })
            .collect();
        assert_eq!(kinds, vec!["start", "pause", "resume", "stop"]);
    }
}
