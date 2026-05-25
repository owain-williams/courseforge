//! Pluggable ASR backends.
//!
//! Like the recorder, this module abstracts *how words come out of a Segment
//! file*. In production on macOS we hand back a [`whisper::WhisperBackend`]
//! that runs local whisper.cpp inference; on other platforms (and in unit
//! tests that wire the manager up directly) the [`fake`] backend keeps the
//! queue, persistence, and UI honest.

use std::path::Path;
use crate::core::error::Result;
use crate::core::transcript::Word;

pub mod fake;
pub mod model;
#[cfg(target_os = "macos")]
pub mod whisper;

/// A backend turns a Segment file into a stream of timestamped words.
///
/// `transcribe` blocks the calling thread; the `TranscriptionManager` runs
/// it from a single background worker so the UI thread is never blocked.
/// `progress` lets backends emit fractional progress (0.0..=1.0) so the UI
/// can show a bar — implementations that can't easily estimate progress
/// emit `0.0` at the start and `1.0` at the end.
pub trait TranscriberBackend: Send + Sync {
    fn transcribe(
        &self,
        segment_path: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<Word>>;
}

/// Sink the backend pushes progress fractions to. Wrapped instead of a plain
/// closure so the trait stays object-safe.
pub trait ProgressSink: Send + Sync {
    fn report(&self, fraction: f64);
}

/// No-op sink for callers that don't care about progress (tests, mainly).
pub struct NullProgressSink;
impl ProgressSink for NullProgressSink {
    fn report(&self, _fraction: f64) {}
}

/// Pick a backend for the current platform. macOS production builds get the
/// real whisper.cpp backend with the default model variant; everywhere else
/// (and in headless test setups that wire the manager up directly) falls
/// back to the scripted fake so the rest of the app still links and runs.
pub fn default_backend() -> Box<dyn TranscriberBackend> {
    #[cfg(target_os = "macos")]
    {
        if let Some(dir) = whisper::WhisperBackend::default_models_dir() {
            return Box::new(whisper::WhisperBackend::new(
                dir,
                model::WhisperModel::BaseEn,
            ));
        }
    }
    Box::new(fake::FakeTranscriberBackend::default())
}
