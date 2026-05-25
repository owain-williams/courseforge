//! On-disk store for downloaded whisper.cpp model files.
//!
//! Models live in `~/Library/Application Support/Courseforge/models/` so they
//! survive across Courseforge upgrades and are shared across every Course
//! Folder on this Mac. Each model is downloaded once; subsequent
//! transcriptions reuse the file on disk (NFR-1, NFR-2).
//!
//! Downloads stream to a `<filename>.part` sibling and are renamed atomically
//! when complete, so a process crash or quit mid-download leaves a partial
//! file the next run can resume into via an HTTP `Range` request.

use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::core::error::{CoreError, Result};
use crate::transcriber::ProgressSink;

/// Which whisper.cpp model variant to use. `BaseEn` is the v1 default —
/// ~142 MB on disk, English-only, the sweet spot for solo-creator speech.
/// `TinyEn` is exposed for fast tests and as a fallback override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhisperModel {
    TinyEn,
    BaseEn,
}

impl WhisperModel {
    /// Filename on disk. Matches the ggerganov/whisper.cpp release naming so
    /// the URL and on-disk file share a basename.
    pub fn filename(self) -> &'static str {
        match self {
            WhisperModel::TinyEn => "ggml-tiny.en.bin",
            WhisperModel::BaseEn => "ggml-base.en.bin",
        }
    }

    /// Hugging Face URL the file is fetched from. Resolves a stable, CDN-
    /// backed mirror of the official `ggerganov/whisper.cpp` model dump.
    pub fn download_url(self) -> &'static str {
        match self {
            WhisperModel::TinyEn => {
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin"
            }
            WhisperModel::BaseEn => {
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"
            }
        }
    }
}

/// Filesystem-backed store of downloaded models.
pub struct ModelStore {
    dir: PathBuf,
}

impl ModelStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The location any installed copy of `model` would live at.
    pub fn path_for(&self, model: WhisperModel) -> PathBuf {
        self.dir.join(model.filename())
    }

    /// Sibling `.part` file used while a download is in flight.
    fn part_path_for(&self, model: WhisperModel) -> PathBuf {
        self.dir.join(format!("{}.part", model.filename()))
    }

    /// True if the model is already fully downloaded.
    pub fn is_present(&self, model: WhisperModel) -> bool {
        self.path_for(model).is_file()
    }

    /// Make sure the model is on disk, downloading it if missing. Returns the
    /// absolute path to the model file. A pre-existing `.part` is resumed
    /// rather than redownloaded.
    pub fn ensure_downloaded(
        &self,
        model: WhisperModel,
        progress: &dyn ProgressSink,
    ) -> Result<PathBuf> {
        let final_path = self.path_for(model);
        if final_path.is_file() {
            // Treat the model as already done so subsequent calls remain
            // offline (NFR-1 / NFR-2).
            progress.report(1.0);
            return Ok(final_path);
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| CoreError::Io {
            path: self.dir.clone(),
            source: e,
        })?;

        let part_path = self.part_path_for(model);
        let existing = part_bytes_on_disk(&part_path);
        let (mut reader, total_bytes) =
            open_resumable_download(model.download_url(), existing)?;

        write_resumable_stream(
            &mut reader,
            &part_path,
            &final_path,
            existing,
            total_bytes,
            progress,
        )?;
        Ok(final_path)
    }
}

fn part_bytes_on_disk(part_path: &Path) -> u64 {
    part_path.metadata().map(|m| m.len()).unwrap_or(0)
}

/// Copy `reader` into `<final_path>.part` (appending, so a previously
/// interrupted download resumes), then rename to `final_path`.
///
/// Pulled out as its own function so it's testable without an HTTP server:
/// the unit tests feed it a `Cursor` and assert the on-disk layout.
pub(crate) fn write_resumable_stream<R: Read>(
    reader: &mut R,
    part_path: &Path,
    final_path: &Path,
    already_have: u64,
    total_bytes: Option<u64>,
    progress: &dyn ProgressSink,
) -> Result<()> {
    if let Some(parent) = part_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }

    let mut part_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(part_path)
        .map_err(|e| CoreError::Io {
            path: part_path.to_path_buf(),
            source: e,
        })?;

    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded = already_have;
    let mut received_this_run: u64 = 0;

    // Emit an opening tick so the UI moves immediately when we start.
    report_fraction(progress, downloaded, total_bytes);

    loop {
        let n = reader.read(&mut buf).map_err(|e| CoreError::Transcriber(format!(
            "model download read failed: {e}"
        )))?;
        if n == 0 {
            break;
        }
        part_file.write_all(&buf[..n]).map_err(|e| CoreError::Io {
            path: part_path.to_path_buf(),
            source: e,
        })?;
        downloaded += n as u64;
        received_this_run += n as u64;
        report_fraction(progress, downloaded, total_bytes);
    }

    // Refuse to declare a download successful if the server returned zero
    // bytes and we still have nothing on disk — that almost certainly means
    // the URL is wrong or HF served an empty response.
    if already_have == 0 && received_this_run == 0 {
        return Err(CoreError::Transcriber(
            "model download produced zero bytes".into(),
        ));
    }
    // Flush before rename so the rename is durable.
    part_file.flush().map_err(|e| CoreError::Io {
        path: part_path.to_path_buf(),
        source: e,
    })?;
    drop(part_file);

    std::fs::rename(part_path, final_path).map_err(|e| CoreError::Io {
        path: final_path.to_path_buf(),
        source: e,
    })?;
    progress.report(1.0);
    Ok(())
}

fn report_fraction(progress: &dyn ProgressSink, downloaded: u64, total: Option<u64>) {
    if let Some(total) = total {
        if total > 0 {
            let frac = (downloaded as f64 / total as f64).clamp(0.0, 1.0);
            progress.report(frac);
            return;
        }
    }
    // Indeterminate — at least move off zero so the UI shows activity.
    progress.report(0.0);
}

/// Issue a (resumable) HTTP GET and hand back a streaming reader plus the
/// total byte count once the resume range is accounted for.
fn open_resumable_download(
    url: &str,
    already_have: u64,
) -> Result<(Box<dyn Read + Send + 'static>, Option<u64>)> {
    let mut req = ureq::get(url);
    if already_have > 0 {
        req = req.header("Range", &format!("bytes={already_have}-"));
    }
    let response = req.call().map_err(|e| CoreError::Transcriber(format!(
        "model download HTTP error for {url}: {e}"
    )))?;

    let remaining = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    let total = remaining.map(|r| r + already_have);

    let reader = response.into_body().into_reader();
    Ok((Box::new(reader), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::Mutex;

    struct CapturingSink {
        seen: Mutex<Vec<f64>>,
    }
    impl CapturingSink {
        fn new() -> Self {
            Self { seen: Mutex::new(Vec::new()) }
        }
    }
    impl ProgressSink for CapturingSink {
        fn report(&self, fraction: f64) {
            self.seen.lock().unwrap().push(fraction);
        }
    }

    #[test]
    fn path_for_resolves_under_the_models_dir() {
        let store = ModelStore::new(PathBuf::from("/some/models"));
        assert_eq!(
            store.path_for(WhisperModel::BaseEn),
            PathBuf::from("/some/models/ggml-base.en.bin"),
        );
    }

    #[test]
    fn is_present_returns_false_when_the_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        assert!(!store.is_present(WhisperModel::BaseEn));
    }

    #[test]
    fn is_present_returns_true_when_the_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"pretend-model").unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        assert!(store.is_present(WhisperModel::BaseEn));
    }

    #[test]
    fn write_resumable_stream_creates_part_file_then_renames_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.bin.part");
        let final_path = dir.path().join("m.bin");
        let data = b"hello-whisper".to_vec();
        let mut reader = Cursor::new(data.clone());

        write_resumable_stream(
            &mut reader,
            &part,
            &final_path,
            0,
            Some(data.len() as u64),
            &CapturingSink::new(),
        )
        .unwrap();

        assert!(!part.is_file(), ".part should be renamed away");
        assert!(final_path.is_file(), "final file should exist after rename");
        assert_eq!(std::fs::read(&final_path).unwrap(), data);
    }

    #[test]
    fn write_resumable_stream_appends_to_an_existing_part_file_for_resume() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.bin.part");
        let final_path = dir.path().join("m.bin");
        let already = b"AAAAA".to_vec();
        std::fs::write(&part, &already).unwrap();

        let rest = b"BBBBB".to_vec();
        let mut reader = Cursor::new(rest.clone());
        write_resumable_stream(
            &mut reader,
            &part,
            &final_path,
            already.len() as u64,
            Some((already.len() + rest.len()) as u64),
            &CapturingSink::new(),
        )
        .unwrap();

        // After resume, the final file is the concatenation of what we had
        // and what we received.
        let on_disk = std::fs::read(&final_path).unwrap();
        assert_eq!(on_disk, b"AAAAABBBBB");
    }

    #[test]
    fn write_resumable_stream_reports_fractional_progress_when_total_is_known() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.bin.part");
        let final_path = dir.path().join("m.bin");
        let data = vec![0u8; 200_000]; // > 64KB so multiple ticks happen
        let mut reader = Cursor::new(data.clone());
        let sink = CapturingSink::new();

        write_resumable_stream(
            &mut reader,
            &part,
            &final_path,
            0,
            Some(data.len() as u64),
            &sink,
        )
        .unwrap();

        let seen = sink.seen.lock().unwrap().clone();
        assert!(seen.len() > 2, "expected multiple progress ticks, got {seen:?}");
        assert!((0.0 - seen[0]).abs() < f64::EPSILON, "first tick should be 0.0");
        assert!((seen.last().copied().unwrap() - 1.0).abs() < f64::EPSILON);
        // Monotonic non-decreasing.
        for w in seen.windows(2) {
            assert!(w[1] >= w[0], "progress went backwards: {seen:?}");
        }
    }

    #[test]
    fn write_resumable_stream_errors_when_the_server_returns_zero_bytes_with_no_part() {
        let dir = tempfile::tempdir().unwrap();
        let part = dir.path().join("m.bin.part");
        let final_path = dir.path().join("m.bin");
        let mut reader = Cursor::new(Vec::<u8>::new());

        let err = write_resumable_stream(
            &mut reader,
            &part,
            &final_path,
            0,
            None,
            &CapturingSink::new(),
        )
        .unwrap_err();
        match err {
            CoreError::Transcriber(msg) => assert!(msg.contains("zero bytes"), "got: {msg}"),
            other => panic!("expected Transcriber error, got {other:?}"),
        }
        assert!(!final_path.is_file());
    }

    #[test]
    fn ensure_downloaded_is_a_noop_when_the_model_is_already_present() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ggml-base.en.bin"), b"already-here").unwrap();
        let store = ModelStore::new(dir.path().to_path_buf());
        let sink = CapturingSink::new();
        let path = store
            .ensure_downloaded(WhisperModel::BaseEn, &sink)
            .expect("present model should short-circuit without network");
        assert_eq!(path, dir.path().join("ggml-base.en.bin"));
        // We should have emitted at least the terminal 1.0 tick.
        let seen = sink.seen.lock().unwrap().clone();
        assert_eq!(seen.last().copied(), Some(1.0));
    }
}
