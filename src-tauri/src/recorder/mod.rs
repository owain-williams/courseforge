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
use crate::core::capture::{CaptureRequest, EndedReason};
use crate::core::error::Result;

pub mod fake;

#[cfg(target_os = "macos")]
pub mod sck_mac;

/// Per-source outcome reported back from the backend on Stop (or on a
/// mid-Take failure that ends one source while the others continue).
///
/// `ended_reason` is `Normal` when the source ran to clean Stop, or
/// `SourceFailed` when the source's writer / capture session errored
/// mid-Take. `ended_at` is the ISO-8601 timestamp the failure landed at
/// (only populated for `SourceFailed`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceOutcome {
    pub segment_id: String,
    pub ended_reason: EndedReason,
    pub ended_at: Option<String>,
}

/// One per-source slot in a Take — pairs the `CaptureRequest` (role +
/// device + composition defaults) with the segment id and partial path
/// the manager has allocated for this source. The backend writes its
/// per-source AVAssetWriter output to `partial_path` and the manager
/// later renames it to its final form during `finalize_segment`.
#[derive(Debug, Clone)]
pub struct TakeSource {
    pub request: CaptureRequest,
    pub segment_id: String,
    pub partial_path: PathBuf,
}

/// One Take's worth of sources, plus the take-grouping id shared across
/// every Segment in the Take. Per ADR-0002 every per-Segment sidecar in
/// the Take echoes `take_id` back so the editor can group them.
#[derive(Debug, Clone)]
pub struct TakeRequest {
    pub take_id: String,
    pub sources: Vec<TakeSource>,
}

/// Factory for per-session recording handles. Lives in Tauri-managed state
/// as a `Box<dyn RecorderBackend>` so commands can mint a fresh
/// `ActiveRecording` per `start_recording` call without caring which backend
/// is wired up.
pub trait RecorderBackend: Send + Sync {
    /// Start a Take (one or more sources captured together). Phase 2's
    /// macOS backend fans out to N AVAssetWriters under one shared
    /// `CMSampleBuffer` PTS clock; non-macOS falls back to the in-memory
    /// fake. Atomic Start: any single-source initialisation failure
    /// tears the whole Take down before returning and leaves no partial
    /// files behind.
    fn start(&self, take: TakeRequest) -> Result<Box<dyn ActiveRecording>>;
}

/// One in-progress capture. The handle is interior-mutable so callers can
/// keep it behind a shared lock; concrete impls coordinate their internal
/// capture-stream state.
pub trait ActiveRecording: Send + Sync {
    /// Atomic Pause across every writer in the Take — once this returns,
    /// no further sample appends happen until `resume` is called.
    /// Implemented in Phase 2 via a shared atomic the per-source sample
    /// delegates consult before calling `appendSampleBuffer`.
    fn pause(&self) -> Result<()>;
    fn resume(&self) -> Result<()>;
    /// Finalise every in-progress `.partial.*` in parallel and return one
    /// per-source outcome per slot. After `stop` returns, no further
    /// pause/resume/stop calls are valid; the manager drops the handle.
    /// The returned `Vec<SourceOutcome>` has one entry per source in the
    /// original `TakeRequest` so the manager can emit per-source sidecars.
    fn stop(&self) -> Result<Vec<SourceOutcome>>;
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
