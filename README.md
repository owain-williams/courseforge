# courseforge

A local-first macOS desktop app for solo course creators — ideate, record,
edit, and export online video courses end to end.

See [`CONTEXT.md`](./CONTEXT.md) for the domain glossary and
[`docs/adr/`](./docs/adr/) for architectural decisions.

## Local ASR / transcription

Recordings are auto-transcribed by a local [whisper.cpp][whisper-cpp] model
running on-device — no audio leaves the Mac.

- **Crate**: [`whisper-rs`][whisper-rs] (Rust bindings to whisper.cpp,
  Metal-accelerated on Apple Silicon). Chosen over shelling out to a
  bundled CLI so the worker can keep the model loaded in memory between
  Segments and surface progress via callback rather than parsed stderr.
- **Default model**: `ggml-base.en.bin` (~142 MB). Good quality/speed
  tradeoff for English-language solo creators. Override per-call to swap
  in `tiny.en` for faster iteration or a larger variant for accuracy.
- **On-disk path**: `~/Library/Application Support/Courseforge/models/`.
  Lives outside the Course Folder so transcripts travel with the Course
  (NFR-6 portability) but the model does not. First transcription on a
  clean Mac downloads the file with visible progress and resumes on quit
  via a `.part` sibling; subsequent runs reuse the file (NFR-1, NFR-2).

The full pipeline (recorder → ffmpeg audio extract → whisper.cpp → on-disk
`transcript.json`) flows through `TranscriberBackend`, so unit tests can
substitute `FakeTranscriberBackend` to exercise the queue, persistence,
and UI without touching the network or GPU.

[whisper-cpp]: https://github.com/ggerganov/whisper.cpp
[whisper-rs]: https://crates.io/crates/whisper-rs

## Running the real-whisper integration test

The bundled `tests/fixtures/hello-world.wav` is a sub-MB 16 kHz mono clip.
Driving it through the real backend is gated behind `#[ignore]` because the
first run downloads ~75 MB (the `tiny.en` model used by tests):

```sh
cargo test -p courseforge --release \
  --test whisper_real_transcription -- --ignored
```

Set `COURSEFORGE_TEST_MODEL_DIR` to override where the test caches its
model — the default is `~/Library/Caches/courseforge-test-models/`, kept
deliberately separate from the production `Application Support` path so
tests never pollute the user's real model store.
