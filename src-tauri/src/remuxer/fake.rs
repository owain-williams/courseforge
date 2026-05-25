//! A test-only `Remuxer` that just copies bytes. Used in unit and
//! integration tests that drive the recording pipeline with a
//! `FakeRecorderBackend` writing placeholder content — running real ffmpeg
//! against `b"FAKE-PARTIAL-MKV"` would fail, so we substitute a pure
//! file-copy that preserves the contract (an MP4 lands at `dst`) without
//! needing valid container bytes.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::core::error::{CoreError, Result};
use super::Remuxer;

/// Logs each (src, dst) pair so tests can assert on it. The default impl
/// shares an empty log; clone via `shared_log` if a test needs to peek.
#[derive(Default, Clone)]
pub struct FakeRemuxer {
    pub calls: Arc<Mutex<Vec<(std::path::PathBuf, std::path::PathBuf)>>>,
}

impl Remuxer for FakeRemuxer {
    fn remux_to_mp4(&self, src: &Path, dst: &Path) -> Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push((src.to_path_buf(), dst.to_path_buf()));
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }
        std::fs::copy(src, dst).map_err(|e| CoreError::Io {
            path: dst.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remux_to_mp4_copies_bytes_and_logs_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.partial.mkv");
        let dst = dir.path().join("out.mp4");
        std::fs::write(&src, b"some bytes").unwrap();

        let r = FakeRemuxer::default();
        r.remux_to_mp4(&src, &dst).unwrap();

        assert_eq!(std::fs::read(&dst).unwrap(), b"some bytes");
        let calls = r.calls.lock().unwrap().clone();
        assert_eq!(calls, vec![(src, dst)]);
    }

    #[test]
    fn remux_to_mp4_creates_the_destination_parent_if_missing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("in.partial.mkv");
        let dst = dir.path().join("nested/deep/out.mp4");
        std::fs::write(&src, b"x").unwrap();

        FakeRemuxer::default().remux_to_mp4(&src, &dst).unwrap();
        assert!(dst.is_file());
    }
}
