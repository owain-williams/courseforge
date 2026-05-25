//! Pluggable recording backends.
//!
//! The domain layer (`core::recording`) decides *what state a session is in*;
//! this module decides *how bytes actually reach disk*. Trait-based so the
//! recording-flow tests can drive the state machine with an in-memory fake
//! and so the macOS ffmpeg backend stays compiled out on other platforms.

use std::path::PathBuf;
use crate::core::error::Result;
use crate::core::permissions::CaptureSources;

pub mod fake;

#[cfg(target_os = "macos")]
pub mod ffmpeg_mac;

/// Factory for per-session recording handles. Lives in Tauri-managed state
/// as a `Box<dyn RecorderBackend>` so commands can mint a fresh
/// `ActiveRecording` per `start_recording` call without caring which backend
/// is wired up.
pub trait RecorderBackend: Send + Sync {
    fn start(&self, partial_path: PathBuf, sources: CaptureSources)
        -> Result<Box<dyn ActiveRecording>>;
}

/// One in-progress capture. The handle is interior-mutable so callers can
/// keep it behind a shared lock; concrete impls coordinate their internal
/// child-process / capture-stream state.
pub trait ActiveRecording: Send + Sync {
    fn pause(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    /// Finalise the in-progress `.partial.mkv` so it's safe to rename / hand
    /// to the user. After `stop` returns Ok, no further pause/resume/stop
    /// calls are valid; the manager drops the handle.
    fn stop(&self) -> Result<()>;
}

/// Pick a real backend for the current platform, or a fake on non-macOS so
/// the rest of the app still links and the UI can be smoke-tested.
pub fn default_backend() -> Box<dyn RecorderBackend> {
    #[cfg(target_os = "macos")]
    {
        Box::new(ffmpeg_mac::FfmpegMacBackend::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(fake::FakeRecorderBackend::default())
    }
}
