//! Pluggable ASR backends.
//!
//! Like the recorder, this module abstracts *how words come out of a Segment
//! file*. The default v1 plan (HITL on issue #8) is local whisper.cpp via a
//! follow-up PR; meanwhile a `FakeTranscriberBackend` keeps the queue,
//! persistence, and UI honest under test.

use std::path::Path;
use crate::core::error::Result;
use crate::core::transcript::Word;

pub mod fake;

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

/// Pick a backend for production. For now we always hand back the fake —
/// the real whisper.cpp impl lands in a follow-up (see issue #8 thread).
pub fn default_backend() -> Box<dyn TranscriberBackend> {
    Box::new(fake::FakeTranscriberBackend::default())
}
