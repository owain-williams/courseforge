//! Real ASR backend: local whisper.cpp via the `whisper-rs` Rust bindings.
//!
//! Lifecycle per `transcribe` call:
//! 1. Ensure the chosen [`WhisperModel`] is downloaded into the on-disk
//!    `ModelStore`. First call per Mac downloads with visible progress;
//!    subsequent calls reuse the file (NFR-1, NFR-2).
//! 2. Extract the Segment's audio track into a 16 kHz mono WAV via the
//!    `ffmpeg` binary already on `$PATH`. Whisper requires that exact
//!    format and we already rely on ffmpeg for capture.
//! 3. Lazy-load a `WhisperContext` on first transcription and keep it in
//!    memory between calls — model load is the expensive part, so the
//!    second-and-onward transcription latency is just inference.
//! 4. Run inference with `max_len=1` + `split_on_word=true` + token-level
//!    timestamps so each WhisperSegment maps to one word with start/end
//!    in centiseconds, which we convert to seconds for the [`Word`] type.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, Once};

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
};

use crate::core::error::{CoreError, Result};
use crate::core::transcript::Word;
use crate::transcriber::model::{ModelStore, WhisperModel};
use crate::transcriber::{ProgressSink, TranscriberBackend};

/// Backend that runs whisper.cpp locally. Keep one of these in
/// Tauri-managed state and clone via Arc — the `Mutex<Option<...>>` makes
/// the lazy-loaded model shareable across the single worker thread.
pub struct WhisperBackend {
    store: ModelStore,
    model: WhisperModel,
    context: Mutex<Option<WhisperContext>>,
}

static SILENCE_WHISPER_LOGS: Once = Once::new();

impl WhisperBackend {
    /// Build a backend that downloads (if needed) into `models_dir`.
    pub fn new(models_dir: PathBuf, model: WhisperModel) -> Self {
        // Route whisper.cpp + ggml's own printf-style logs through Rust's
        // logging hooks instead of letting them spam stderr. With no
        // `log`/`tracing` backend wired up, this effectively silences them.
        SILENCE_WHISPER_LOGS.call_once(|| {
            whisper_rs::install_logging_hooks();
        });
        Self {
            store: ModelStore::new(models_dir),
            model,
            context: Mutex::new(None),
        }
    }

    /// Default location for the model store under macOS conventions:
    /// `~/Library/Application Support/Courseforge/models/`.
    pub fn default_models_dir() -> Option<PathBuf> {
        dirs::data_dir().map(|d| d.join("Courseforge").join("models"))
    }
}

impl TranscriberBackend for WhisperBackend {
    fn transcribe(
        &self,
        segment_path: &Path,
        progress: &dyn ProgressSink,
    ) -> Result<Vec<Word>> {
        // The model download dominates the first run; report 0.0 first so the
        // UI moves immediately, then hand the sink to `ensure_downloaded` so
        // the progress bar tracks bytes-on-the-wire rather than freezing.
        progress.report(0.0);
        let model_path = self.store.ensure_downloaded(self.model, progress)?;

        let wav_path = extract_wav_16k_mono(segment_path)?;
        let result = (|| -> Result<Vec<Word>> {
            let samples = read_wav_as_mono_f32(&wav_path)?;
            let mut guard = self.context.lock().unwrap();
            if guard.is_none() {
                let ctx = WhisperContext::new_with_params(
                    model_path
                        .to_str()
                        .ok_or_else(|| CoreError::Transcriber(format!(
                            "model path is not valid utf-8: {}",
                            model_path.display()
                        )))?,
                    WhisperContextParameters::default(),
                )
                .map_err(|e| CoreError::Transcriber(format!(
                    "failed to load whisper model at {}: {e}",
                    model_path.display()
                )))?;
                *guard = Some(ctx);
            }
            let ctx = guard.as_ref().expect("just loaded above");
            run_inference(ctx, &samples)
        })();

        // Best-effort cleanup of the temp WAV regardless of inference outcome.
        let _ = std::fs::remove_file(&wav_path);

        let words = result?;
        progress.report(1.0);
        Ok(words)
    }
}

/// Pull the audio out of a Segment file (any container ffmpeg can read) and
/// write a 16 kHz mono 16-bit WAV next to the system temp dir. The temp file
/// is cleaned up by `transcribe` once whisper-rs has consumed it.
fn extract_wav_16k_mono(input: &Path) -> Result<PathBuf> {
    let ffmpeg = find_ffmpeg().ok_or_else(|| {
        CoreError::Transcriber(
            "ffmpeg not found — install it (e.g. `brew install ffmpeg`) and try again".into(),
        )
    })?;

    let out = std::env::temp_dir().join(format!(
        "courseforge-asr-{}.wav",
        uuid::Uuid::new_v4().simple()
    ));

    let status = Command::new(&ffmpeg)
        .args([
            "-hide_banner",
            "-loglevel", "error",
            "-y",
            "-i", input.to_str().ok_or_else(|| CoreError::Transcriber(format!(
                "segment path is not valid utf-8: {}",
                input.display()
            )))?,
            "-ac", "1",
            "-ar", "16000",
            "-c:a", "pcm_s16le",
            "-f", "wav",
            out.to_str().expect("temp path was just constructed with utf-8 chars"),
        ])
        .status()
        .map_err(|e| CoreError::Transcriber(format!("failed to spawn ffmpeg: {e}")))?;
    if !status.success() {
        let _ = std::fs::remove_file(&out);
        return Err(CoreError::Transcriber(format!(
            "ffmpeg audio extraction failed with status {status} for {}",
            input.display()
        )));
    }
    Ok(out)
}

fn find_ffmpeg() -> Option<PathBuf> {
    // Mirror the recorder's lookup so a missing ffmpeg surfaces consistently.
    let candidates = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(out) = Command::new("/usr/bin/which").arg("ffmpeg").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

/// Read a WAV file into a `Vec<f32>` of mono samples normalised to [-1, 1].
/// whisper.cpp insists on 16 kHz mono f32; we don't rescale here because
/// `extract_wav_16k_mono` already guarantees it.
fn read_wav_as_mono_f32(wav_path: &Path) -> Result<Vec<f32>> {
    let reader = hound::WavReader::open(wav_path).map_err(|e| {
        CoreError::Transcriber(format!("failed to open wav {}: {e}", wav_path.display()))
    })?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 {
        return Err(CoreError::Transcriber(format!(
            "expected 16 kHz wav after extraction; got {} Hz",
            spec.sample_rate
        )));
    }
    if spec.channels != 1 {
        return Err(CoreError::Transcriber(format!(
            "expected mono wav after extraction; got {} channels",
            spec.channels
        )));
    }
    let samples_i16: std::result::Result<Vec<i16>, _> = reader.into_samples::<i16>().collect();
    let samples_i16 = samples_i16.map_err(|e| {
        CoreError::Transcriber(format!("failed to read wav samples from {}: {e}", wav_path.display()))
    })?;
    let mut audio = vec![0.0f32; samples_i16.len()];
    whisper_rs::convert_integer_to_float_audio(&samples_i16, &mut audio).map_err(|e| {
        CoreError::Transcriber(format!("sample conversion failed: {e}"))
    })?;
    Ok(audio)
}

fn run_inference(ctx: &WhisperContext, samples: &[f32]) -> Result<Vec<Word>> {
    let mut state = ctx
        .create_state()
        .map_err(|e| CoreError::Transcriber(format!("failed to create whisper state: {e}")))?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 0 });
    // English-only models (`*.en.bin`) are still happier with the language
    // pinned explicitly; multilingual models would need this dropped.
    params.set_language(Some("en"));
    params.set_translate(false);
    // Quiet — whisper.cpp will otherwise print to stdout from C.
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    // Force one-word segments with word-level timestamps so we can map each
    // WhisperSegment directly to a `Word`.
    params.set_token_timestamps(true);
    params.set_split_on_word(true);
    params.set_max_len(1);

    state
        .full(params, samples)
        .map_err(|e| CoreError::Transcriber(format!("whisper inference failed: {e}")))?;

    let mut words = Vec::new();
    for seg in state.as_iter() {
        let text = seg
            .to_str_lossy()
            .map_err(|e| CoreError::Transcriber(format!("segment text decode failed: {e}")))?
            .to_string();
        // Centiseconds → seconds.
        let start = seg.start_timestamp() as f64 / 100.0;
        let end = seg.end_timestamp() as f64 / 100.0;
        // Skip empty / whitespace-only segments — whisper sometimes emits a
        // padding token at the boundaries.
        if text.trim().is_empty() {
            continue;
        }
        words.push(Word { start, end, text });
    }
    Ok(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_models_dir_lives_under_application_support_on_macos() {
        let dir = WhisperBackend::default_models_dir().expect("home dir available");
        // We can't pin to an exact $HOME-dependent path, but we can pin the
        // suffix the rest of the system needs to find files.
        let path_str = dir.to_string_lossy();
        assert!(
            path_str.ends_with("Courseforge/models"),
            "unexpected models dir: {path_str}"
        );
    }
}
