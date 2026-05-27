//! In-memory recorder used by the session-manager tests and as the fallback
//! backend on non-macOS dev machines.
//!
//! Behaviour: `start` touches each source's `.partial.*` file so downstream
//! code sees a real file per slot, and `pause` / `resume` / `stop` just
//! update an `Arc<Mutex>` transition log. Tests can introspect that log to
//! verify the manager called the backend in the right order with the right
//! `TakeRequest`. `stop` returns one [`SourceOutcome`] per slot with
//! `EndedReason::Normal` — issue #37's mid-Take per-source failure path
//! lets tests inject `SourceFailed` outcomes via [`FakeRecorderBackend::fail_source`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::capture::EndedReason;
use crate::core::error::{CoreError, Result};
use super::{ActiveRecording, RecorderBackend, SourceOutcome, TakeRequest};

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
    /// Per-source-id forced outcomes. The next `stop()` call returns
    /// these in place of the default `Normal` outcome for any source whose
    /// segment id is in the map. Used to drive the mid-Take per-source
    /// failure tests deterministically without needing a real backend.
    pub failures: Arc<Mutex<std::collections::HashMap<String, (EndedReason, Option<String>)>>>,
}

impl FakeRecorderBackend {
    pub fn shared_log(&self) -> Arc<Mutex<Vec<FakeEvent>>> {
        self.events.clone()
    }

    /// Returns a clone that shares the same event log + failures map.
    /// Lets tests hand a fresh handle to the manager while keeping a
    /// readable copy for assertions.
    pub fn shared_clone(&self) -> Self {
        Self {
            events: self.events.clone(),
            failures: self.failures.clone(),
        }
    }

    /// Force the named source to come back with `SourceFailed` on the next
    /// `stop()`. Used by the mid-Take per-source failure tests.
    pub fn fail_source(&self, segment_id: &str, ended_at: impl Into<String>) {
        self.failures
            .lock()
            .unwrap()
            .insert(segment_id.into(), (EndedReason::SourceFailed, Some(ended_at.into())));
    }
}

impl RecorderBackend for FakeRecorderBackend {
    fn start(&self, take: TakeRequest) -> Result<Box<dyn ActiveRecording>> {
        let mut paths = Vec::with_capacity(take.sources.len());
        let mut outcomes = Vec::with_capacity(take.sources.len());
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
            outcomes.push(SourceOutcome {
                segment_id: src.segment_id.clone(),
                ended_reason: EndedReason::Normal,
                ended_at: None,
            });
        }

        self.events.lock().unwrap().push(FakeEvent::Start {
            take_id: take.take_id,
            partial_paths: paths.clone(),
        });

        Ok(Box::new(FakeRecording {
            events: self.events.clone(),
            failures: self.failures.clone(),
            _partial_paths: paths,
            outcomes: Mutex::new(outcomes),
            paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }))
    }
}

pub struct FakeRecording {
    events: Arc<Mutex<Vec<FakeEvent>>>,
    failures: Arc<Mutex<std::collections::HashMap<String, (EndedReason, Option<String>)>>>,
    _partial_paths: Vec<PathBuf>,
    /// Cached outcomes — `stop()` consumes these and applies any forced
    /// failures in one pass.
    outcomes: Mutex<Vec<SourceOutcome>>,
    /// Surfaced for tests so they can assert pause was honoured. The real
    /// macOS backend uses an equivalent atomic to gate sample appends.
    pub paused: Arc<std::sync::atomic::AtomicBool>,
}

impl ActiveRecording for FakeRecording {
    fn pause(&self) -> Result<()> {
        self.paused.store(true, std::sync::atomic::Ordering::SeqCst);
        self.events.lock().unwrap().push(FakeEvent::Pause);
        Ok(())
    }
    fn resume(&self) -> Result<()> {
        self.paused.store(false, std::sync::atomic::Ordering::SeqCst);
        self.events.lock().unwrap().push(FakeEvent::Resume);
        Ok(())
    }
    fn stop(&self) -> Result<Vec<SourceOutcome>> {
        self.events.lock().unwrap().push(FakeEvent::Stop);
        let mut outcomes = std::mem::take(&mut *self.outcomes.lock().unwrap());
        let forced = std::mem::take(&mut *self.failures.lock().unwrap());
        for o in &mut outcomes {
            if let Some((reason, ended_at)) = forced.get(&o.segment_id) {
                o.ended_reason = *reason;
                o.ended_at = ended_at.clone();
            }
        }
        Ok(outcomes)
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
            is_transcript_source: false,
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
        let outcomes = h.stop().unwrap();

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
        // Each source comes back with a Normal outcome by default.
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].ended_reason, EndedReason::Normal);
    }

    #[test]
    fn stop_returns_source_failed_for_forced_sources() {
        let dir = tempfile::tempdir().unwrap();
        let backend = FakeRecorderBackend::default();
        let take = take_for(
            dir.path(),
            [(req(SourceRole::Screen), "mov"), (req(SourceRole::Camera), "mov")],
        );
        let segment_ids: Vec<String> = take.sources.iter().map(|s| s.segment_id.clone()).collect();
        let h = backend.start(take).unwrap();
        backend.fail_source(&segment_ids[1], "2026-05-27T01:23:45Z");
        let outcomes = h.stop().unwrap();
        assert_eq!(outcomes[0].ended_reason, EndedReason::Normal);
        assert_eq!(outcomes[1].ended_reason, EndedReason::SourceFailed);
        assert_eq!(
            outcomes[1].ended_at.as_deref(),
            Some("2026-05-27T01:23:45Z")
        );
    }
}
