# Transcriber

Where the auto-transcribe pipeline lives and the decisions baked into it.

## Layout

- `src-tauri/src/transcriber/mod.rs` — `TranscriberBackend` trait,
  `ProgressSink`, and `default_backend()`.
- `src-tauri/src/transcriber/fake.rs` — scripted `FakeTranscriberBackend`.
  Always available so queue / persistence / UI tests don't need a model.
- `src-tauri/src/transcriber/model.rs` — `ModelStore` and `WhisperModel`.
  Pure file-IO + HTTP resume logic; cross-platform; unit-tested in-tree.
- `src-tauri/src/transcriber/whisper.rs` — `WhisperBackend`. macOS-only.
  Wraps `whisper-rs` + an ffmpeg subprocess for 16 kHz mono WAV extraction.
- `src-tauri/src/transcription_manager.rs` — single-worker queue that
  drives the backend, persists `transcript.json`, and emits status events.

## Decisions

- **Crate**: `whisper-rs` (not shelling out to `whisper-cli`). Lets us keep
  one `WhisperContext` loaded in memory between Segments and receive
  progress as a callback instead of parsing stderr.
- **Default model**: `ggml-base.en.bin` (~142 MB). English-only, balanced
  speed vs accuracy for v1. Test code uses `tiny.en` (~75 MB) for speed.
- **Model lifecycle**: lazy-loaded on first `transcribe` call into a
  `Mutex<Option<WhisperContext>>` on the backend struct. The
  `TranscriptionManager` already serialises calls through one worker, so a
  single context is enough.
- **Audio pipeline**: `ffmpeg -ac 1 -ar 16000 -c:a pcm_s16le -f wav` into a
  per-call temp file. Whisper insists on 16 kHz mono; we already depend on
  ffmpeg for capture, so reusing it keeps the surface small.
- **Model storage**: `~/Library/Application Support/Courseforge/models/`,
  resolved via `dirs::data_dir()`. Lives **outside** the Course Folder so
  Course Folders stay portable (NFR-6). First download is resumable via a
  `<filename>.part` sibling + HTTP `Range` request; cached forever after
  (NFR-1, NFR-2 — offline / no-network after first run).

## Failure modes that surface to the UI

All of these flow through `CoreError::Transcriber` → `JobStatus::Failed`
on the job for the Video, which means **Retry** in the UI re-queues a
fresh attempt without losing the Segment:

- ffmpeg missing from `$PATH`
- ffmpeg extraction non-zero exit (corrupt MKV, unreadable codec)
- model download HTTP error, zero-byte response, or interrupted (the
  `.part` survives and the next run resumes)
- model file present but unloadable (whisper-rs `new_with_params` fails)
- inference itself (`state.full(...)`) failing
