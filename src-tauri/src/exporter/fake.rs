//! Test-only `Exporter` that writes a marker MP4 instead of running real
//! ffmpeg. Lets the manager tests and the cross-platform integration tests
//! drive the full export pipeline without depending on a working ffmpeg on
//! the test box.
//!
//! The output is **not** a real MP4 — it's a synthetic marker file whose
//! contents encode the keep-ranges so tests can assert on what the manager
//! handed to the backend without needing ffprobe. Real validation lives in
//! the macOS-only `export_real_ffmpeg.rs` integration test.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::core::error::{CoreError, Result};

use super::{CancelToken, Exporter, ProgressSink};

#[derive(Debug, Clone, PartialEq)]
pub struct FakeExportCall {
    pub src: PathBuf,
    pub dst: PathBuf,
    pub keep_ranges: Vec<(f64, f64)>,
}

#[derive(Default, Clone)]
pub struct FakeExporter {
    /// Every call the manager makes, in order. Useful for asserting that
    /// the manager handed us the right keep-ranges.
    pub calls: Arc<Mutex<Vec<FakeExportCall>>>,
    /// If set, the next `export_mp4` call returns this error instead of
    /// running. Cleared after one use so the same fake can be re-used.
    pub next_error: Arc<Mutex<Option<String>>>,
    /// If set, the backend stalls between progress ticks long enough for a
    /// test to set the cancel flag mid-render. Tests use this to verify
    /// the cancellation path.
    pub stall_between_ticks: Arc<Mutex<bool>>,
}

impl FakeExporter {
    pub fn shared(&self) -> Self {
        self.clone()
    }
    pub fn fail_next(&self, message: impl Into<String>) {
        *self.next_error.lock().unwrap() = Some(message.into());
    }
    pub fn stall(&self) {
        *self.stall_between_ticks.lock().unwrap() = true;
    }
}

impl Exporter for FakeExporter {
    fn export_mp4(
        &self,
        src: &Path,
        keep_ranges: &[(f64, f64)],
        dst: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        self.calls.lock().unwrap().push(FakeExportCall {
            src: src.to_path_buf(),
            dst: dst.to_path_buf(),
            keep_ranges: keep_ranges.to_vec(),
        });

        if let Some(msg) = self.next_error.lock().unwrap().take() {
            return Err(CoreError::Recorder(msg));
        }

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        progress.report(0.0);
        // Emit interior progress ticks so cancellation has something to
        // catch. Real backends do this from ffmpeg's `-progress` pipe.
        let stall = *self.stall_between_ticks.lock().unwrap();
        let ticks = 4;
        for i in 1..ticks {
            if cancel.is_cancelled() {
                // The backend's contract: no half-written output on cancel.
                let _ = std::fs::remove_file(dst);
                return Err(CoreError::ExportCancelled);
            }
            if stall {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            progress.report(i as f64 / ticks as f64);
        }

        // The marker file just spells out what we were asked to do — enough
        // for the manager tests to verify the manager passed the right
        // ranges, without needing ffprobe.
        let mut body = String::from("FAKE-MP4\n");
        for (s, e) in keep_ranges {
            body.push_str(&format!("keep {s:.6} {e:.6}\n"));
        }
        std::fs::write(dst, body.as_bytes()).map_err(|e| CoreError::Io {
            path: dst.to_path_buf(),
            source: e,
        })?;

        progress.report(1.0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exporter::NullProgressSink;

    #[test]
    fn export_writes_a_marker_file_and_records_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("seg.mp4");
        std::fs::write(&src, b"src").unwrap();
        let dst = dir.path().join("out.mp4");
        let exporter = FakeExporter::default();
        let cancel = CancelToken::new();
        exporter
            .export_mp4(&src, &[(0.0, 5.0)], &dst, &NullProgressSink, &cancel)
            .unwrap();
        assert!(dst.is_file());
        let calls = exporter.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].keep_ranges, vec![(0.0, 5.0)]);
    }

    #[test]
    fn export_propagates_a_scripted_failure() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("seg.mp4");
        std::fs::write(&src, b"src").unwrap();
        let dst = dir.path().join("out.mp4");
        let exporter = FakeExporter::default();
        exporter.fail_next("boom");
        let err = exporter
            .export_mp4(&src, &[(0.0, 1.0)], &dst, &NullProgressSink, &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, CoreError::Recorder(s) if s == "boom"));
    }

    #[test]
    fn cancel_set_before_call_drops_output_and_returns_export_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("seg.mp4");
        std::fs::write(&src, b"src").unwrap();
        let dst = dir.path().join("out.mp4");
        let cancel = CancelToken::new();
        cancel.cancel();
        let exporter = FakeExporter::default();
        let err = exporter
            .export_mp4(&src, &[(0.0, 1.0)], &dst, &NullProgressSink, &cancel)
            .unwrap_err();
        assert!(matches!(err, CoreError::ExportCancelled));
        assert!(!dst.exists(), "must not leave half-written output behind");
    }
}
