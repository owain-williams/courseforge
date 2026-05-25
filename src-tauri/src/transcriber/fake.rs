//! Scripted ASR backend used by the transcription-queue tests, the
//! integration test, and (for now) production builds while the real
//! whisper.cpp backend is in flight.
//!
//! The fake "transcribes" by:
//! - Looking up a scripted response keyed by the Segment file path (if any).
//! - Otherwise falling back to a deterministic pseudo-transcript derived from
//!   the file's *bytes* (so the same `.mkv` always yields the same words,
//!   and the integration test can assert on alignment).
//! - Emitting 0.0 → 0.5 → 1.0 progress so the UI's progress bar is wired up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::error::{CoreError, Result};
use crate::core::transcript::Word;
use super::{ProgressSink, TranscriberBackend};

#[derive(Debug, Clone)]
pub enum ScriptedResponse {
    Ok(Vec<Word>),
    Err(String),
}

#[derive(Default)]
pub struct FakeTranscriberBackend {
    pub scripted: Arc<Mutex<HashMap<PathBuf, ScriptedResponse>>>,
    /// Paths the backend was asked to transcribe, in order. Used by tests to
    /// verify the manager calls us correctly.
    pub calls: Arc<Mutex<Vec<PathBuf>>>,
}

impl FakeTranscriberBackend {
    /// Override the response for a specific Segment file. Useful in tests
    /// that want to assert on a specific word list or simulate a failure.
    pub fn script(&self, segment_path: &Path, response: ScriptedResponse) {
        self.scripted
            .lock()
            .unwrap()
            .insert(segment_path.to_path_buf(), response);
    }

    /// Shared handle on the call log so tests can introspect it without
    /// fighting the borrow checker.
    pub fn calls_log(&self) -> Arc<Mutex<Vec<PathBuf>>> {
        self.calls.clone()
    }
}

impl TranscriberBackend for FakeTranscriberBackend {
    fn transcribe(
        &self,
        segment_path: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<Word>> {
        self.calls.lock().unwrap().push(segment_path.to_path_buf());

        progress.report(0.0);

        let scripted = self.scripted.lock().unwrap().get(segment_path).cloned();
        let result = match scripted {
            Some(ScriptedResponse::Ok(words)) => Ok(words),
            Some(ScriptedResponse::Err(message)) => Err(CoreError::Transcriber(message)),
            None => Ok(derive_words(segment_path)?),
        };

        progress.report(1.0);
        result
    }
}

/// Default pseudo-transcript so the queue / persistence still has *something*
/// to write when no script is set. The output is deterministic: we hash the
/// segment file's byte length into a fixed three-word phrase with stable
/// timestamps, so the integration test can compare against the expectation
/// without scripting every test.
fn derive_words(segment_path: &Path) -> Result<Vec<Word>> {
    let meta = std::fs::metadata(segment_path).map_err(|e| CoreError::Io {
        path: segment_path.to_path_buf(),
        source: e,
    })?;
    let len = meta.len();
    // Three words, evenly spaced from 0..1.5s — leave the values stable so
    // the playback-alignment integration test has a known target.
    let words = vec![
        Word { start: 0.00, end: 0.50, text: "Hello".into() },
        Word { start: 0.50, end: 1.00, text: " world".into() },
        Word { start: 1.00, end: 1.50, text: format!(" {}", len) },
    ];
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcriber::NullProgressSink;

    struct CapturingSink {
        seen: Mutex<Vec<f64>>,
    }
    impl ProgressSink for CapturingSink {
        fn report(&self, fraction: f64) {
            self.seen.lock().unwrap().push(fraction);
        }
    }

    fn write_segment(dir: &Path) -> PathBuf {
        let p = dir.join("seg.mkv");
        std::fs::write(&p, b"FAKE-MKV-PAYLOAD").unwrap();
        p
    }

    #[test]
    fn transcribe_emits_scripted_words_when_path_is_keyed() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_segment(dir.path());
        let backend = FakeTranscriberBackend::default();
        let words = vec![Word { start: 0.0, end: 0.4, text: "Hi".into() }];
        backend.script(&seg, ScriptedResponse::Ok(words.clone()));

        let got = backend.transcribe(&seg, &NullProgressSink).unwrap();
        assert_eq!(got, words);
    }

    #[test]
    fn transcribe_returns_transcriber_error_for_scripted_failure() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_segment(dir.path());
        let backend = FakeTranscriberBackend::default();
        backend.script(&seg, ScriptedResponse::Err("model not loaded".into()));

        let err = backend.transcribe(&seg, &NullProgressSink).unwrap_err();
        match err {
            CoreError::Transcriber(msg) => assert_eq!(msg, "model not loaded"),
            other => panic!("expected Transcriber error, got {other:?}"),
        }
    }

    #[test]
    fn transcribe_reports_progress_at_start_and_end_at_minimum() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_segment(dir.path());
        let backend = FakeTranscriberBackend::default();
        let sink = CapturingSink { seen: Mutex::new(Vec::new()) };
        backend.transcribe(&seg, &sink).unwrap();

        let seen = sink.seen.lock().unwrap().clone();
        assert!(seen.first().copied() == Some(0.0));
        assert!(seen.last().copied() == Some(1.0));
    }

    #[test]
    fn transcribe_logs_calls_so_the_manager_can_be_verified() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_segment(dir.path());
        let backend = FakeTranscriberBackend::default();
        backend.transcribe(&seg, &NullProgressSink).unwrap();
        backend.transcribe(&seg, &NullProgressSink).unwrap();

        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], seg);
    }

    #[test]
    fn derive_words_returns_a_deterministic_three_word_phrase() {
        let dir = tempfile::tempdir().unwrap();
        let seg = write_segment(dir.path());
        let backend = FakeTranscriberBackend::default();
        let words = backend.transcribe(&seg, &NullProgressSink).unwrap();

        assert_eq!(words.len(), 3);
        assert_eq!(words[0].text, "Hello");
        assert_eq!(words[1].text, " world");
        // First word's window is contiguous and non-empty.
        assert!(words[0].end > words[0].start);
        // Words are in order and non-overlapping.
        for pair in words.windows(2) {
            assert!(pair[0].end <= pair[1].start, "overlapping: {:?}", pair);
        }
    }
}
