//! Integration test for the real whisper.cpp backend (issue #20).
//!
//! Gated behind `#[ignore]` because it downloads ~75 MB of model on first
//! run and takes a few seconds of CPU/GPU time per run. Once the model is
//! cached on disk it runs offline.
//!
//! Run with:
//! ```sh
//! cargo test -p courseforge --release -- --ignored whisper_real
//! ```
//!
//! The fixture (`tests/fixtures/hello-world.wav`) was generated with macOS
//! `say` + ffmpeg downsample to 16 kHz mono PCM — see the test header for
//! the exact incantation if it ever needs regenerating.

#![cfg(target_os = "macos")]

use std::path::PathBuf;

use courseforge_lib::transcriber::model::WhisperModel;
use courseforge_lib::transcriber::whisper::WhisperBackend;
use courseforge_lib::transcriber::{NullProgressSink, TranscriberBackend};

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("hello-world.wav")
}

/// Where the test caches the model between runs. Honours an env override so
/// CI can point at a pre-warmed cache; otherwise it lives under the
/// per-user cache dir, *not* the production
/// `~/Library/Application Support/Courseforge/models/` path — we don't want
/// tests writing into the user's real model store.
fn test_model_dir() -> PathBuf {
    if let Ok(s) = std::env::var("COURSEFORGE_TEST_MODEL_DIR") {
        return PathBuf::from(s);
    }
    dirs::cache_dir()
        .expect("a cache_dir for tests")
        .join("courseforge-test-models")
}

#[test]
#[ignore = "downloads the whisper model on first run; opt-in via --ignored"]
fn whisper_real_backend_transcribes_a_bundled_wav_to_at_least_one_expected_word() {
    let backend = WhisperBackend::new(test_model_dir(), WhisperModel::TinyEn);
    let words = backend
        .transcribe(&fixture_path(), &NullProgressSink)
        .expect("real backend should succeed on the bundled WAV");

    assert!(!words.is_empty(), "expected at least one word back");
    let joined = words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<String>()
        .to_lowercase();

    // The fixture says "Hello world. This is a test of the auto transcribe
    // pipeline." We're tolerant of tiny.en's quirks: assert that any one of
    // the high-signal words shows up rather than the full sentence.
    let candidates = ["hello", "world", "test", "pipeline"];
    let hit = candidates.iter().any(|c| joined.contains(c));
    assert!(
        hit,
        "expected one of {candidates:?} in transcript, got: {joined:?}"
    );

    // Timestamps should be monotone and inside the fixture's duration. The
    // clip is ~3.5s long; we leave generous headroom for tiny.en jitter.
    for w in &words {
        assert!(
            w.end >= w.start,
            "non-monotone timestamps on word: {w:?}"
        );
        assert!(w.start >= 0.0 && w.end <= 30.0, "implausible word time: {w:?}");
    }
}
