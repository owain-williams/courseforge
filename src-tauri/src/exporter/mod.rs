//! Pluggable export backends.
//!
//! The [`Exporter`] trait renders a Video → final MP4 by reading a *source*
//! Segment and applying a set of keep-ranges (derived from the EDL by
//! [`crate::core::export::keep_ranges`]). The trait deliberately knows
//! nothing about cuts, transcripts, or the Course Folder layout — its job
//! is just "take these byte ranges and produce a clean MP4".
//!
//! Two backends ship:
//! * [`fake`] — used by manager and integration tests; concatenates ranges
//!   into a marker file without needing ffmpeg.
//! * [`ffmpeg_mac`] — the production macOS backend.
//!
//! Progress is reported via a [`ProgressSink`] (same shape as the
//! transcriber's so the UI binding pattern is uniform). Cancellation goes
//! the other direction via a [`CancelToken`]: the manager sets the flag,
//! the backend polls it between work units and exits cleanly. Half-written
//! output is removed by the backend on cancellation so the AC
//! ("cancellation leaves no half-written output") holds even if the user
//! cancels mid-render.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::error::Result;

pub mod fake;

#[cfg(target_os = "macos")]
pub mod ffmpeg_mac;

/// A sink for the backend to report fractional progress (0.0 ..= 1.0).
/// Shape mirrors the transcriber's [`crate::transcriber::ProgressSink`]
/// so the UI's progress-bar pattern is the same here.
pub trait ProgressSink: Send + Sync {
    fn report(&self, fraction: f64);
}

/// No-op sink for callers that don't care about progress (most tests).
pub struct NullProgressSink;
impl ProgressSink for NullProgressSink {
    fn report(&self, _fraction: f64) {}
}

/// Cancellation flag shared between the manager (which sets it) and the
/// backend (which polls it). Cheap to clone; the backing `Arc<AtomicBool>`
/// is the source of truth. Wrapped in a struct so future implementations
/// can swap in something fancier (e.g. a `tokio::sync::Notify`) without
/// changing every backend.
#[derive(Default, Clone)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }
}

/// Render `src` (a finalised Segment `.mp4`) into `dst`, keeping only the
/// listed byte ranges (on the source timeline, in seconds) and dropping
/// everything else. Implementations:
///
/// * MUST drop `dst` if rendering fails or is cancelled, so a re-export
///   into the same path is a clean retry.
/// * MUST poll `cancel` periodically and return [`crate::core::error::CoreError::ExportCancelled`]
///   as soon as it's set.
/// * MUST report progress via `progress` — at minimum 0.0 at the start
///   and 1.0 at the end, so the UI bar moves.
pub trait Exporter: Send + Sync {
    fn export_mp4(
        &self,
        src: &Path,
        keep_ranges: &[(f64, f64)],
        dst: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()>;
}

/// Pick a backend for the current platform. macOS production builds get
/// ffmpeg; everywhere else (and tests that wire the manager up directly)
/// falls back to the fake so the rest of the app still links.
pub fn default_exporter() -> Box<dyn Exporter> {
    #[cfg(target_os = "macos")]
    {
        Box::new(ffmpeg_mac::FfmpegMacExporter::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(fake::FakeExporter::default())
    }
}
