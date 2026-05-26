//! Pluggable recording backends.
//!
//! The domain layer (`core::recording`) decides *what state a session is in*;
//! this module decides *how bytes actually reach disk*. Trait-based so the
//! recording-flow tests can drive the state machine with an in-memory fake
//! and so the macOS in-process backend stays compiled out on other platforms.
//!
//! Per ADR-0002 the backend accepts `Vec<CaptureRequest>` so a single Take
//! can name N sources. Phase 1 only exercises one or two requests (screen
//! plus optional microphone, matching v1 behaviour) — the fan-out to one
//! `AVAssetWriter` per source lands in Phase 2.

use std::path::PathBuf;
use crate::core::capture::CaptureRequest;
use crate::core::error::Result;

pub mod fake;

#[cfg(target_os = "macos")]
pub mod sck_mac;

/// Factory for per-session recording handles. Lives in Tauri-managed state
/// as a `Box<dyn RecorderBackend>` so commands can mint a fresh
/// `ActiveRecording` per `start_recording` call without caring which backend
/// is wired up.
pub trait RecorderBackend: Send + Sync {
    /// Start a recording. `partial_path` is the `.partial.mov` the backend
    /// should stream to (the manager allocates one path per Take); the
    /// final rename lands at `finalize_segment` time. `requests` describes
    /// the sources to capture; `take_id` is the per-Take grouping id the
    /// backend echoes back into per-Segment sidecars.
    fn start(
        &self,
        partial_path: PathBuf,
        requests: Vec<CaptureRequest>,
        take_id: String,
    ) -> Result<Box<dyn ActiveRecording>>;
}

/// One in-progress capture. The handle is interior-mutable so callers can
/// keep it behind a shared lock; concrete impls coordinate their internal
/// capture-stream state.
pub trait ActiveRecording: Send + Sync {
    fn pause(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    /// Finalise the in-progress `.partial.mov` so it's safe to rename / hand
    /// to the user. After `stop` returns Ok, no further pause/resume/stop
    /// calls are valid; the manager drops the handle.
    fn stop(&self) -> Result<()>;
}

/// Pick a real backend for the current platform, or a fake on non-macOS so
/// the rest of the app still links and the UI can be smoke-tested.
pub fn default_backend() -> Box<dyn RecorderBackend> {
    #[cfg(target_os = "macos")]
    {
        Box::new(sck_mac::SckMacBackend::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(fake::FakeRecorderBackend::default())
    }
}
