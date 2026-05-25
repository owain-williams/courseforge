//! macOS remuxer backed by an `ffmpeg` subprocess. Runs `-c copy` so neither
//! the H.264 video stream nor the AAC audio stream is re-encoded — only the
//! container is rewritten. `-movflags +faststart` puts the `moov` atom at
//! the head of the file so WebKit can start playback before the whole
//! file has been read from disk.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::error::{CoreError, Result};
use super::Remuxer;

pub struct FfmpegMacRemuxer {
    ffmpeg_path: Option<PathBuf>,
}

impl Default for FfmpegMacRemuxer {
    fn default() -> Self {
        Self { ffmpeg_path: find_ffmpeg() }
    }
}

fn find_ffmpeg() -> Option<PathBuf> {
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

impl Remuxer for FfmpegMacRemuxer {
    fn remux_to_mp4(&self, src: &Path, dst: &Path) -> Result<()> {
        let ffmpeg = self.ffmpeg_path.as_ref().ok_or_else(|| {
            CoreError::Recorder(
                "ffmpeg not found — install it (e.g. `brew install ffmpeg`) and try again"
                    .into(),
            )
        })?;
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        let src_arg = src.to_str().ok_or_else(|| CoreError::Recorder(format!(
            "segment path is not valid utf-8: {}",
            src.display()
        )))?;
        let dst_arg = dst.to_str().ok_or_else(|| CoreError::Recorder(format!(
            "segment dest path is not valid utf-8: {}",
            dst.display()
        )))?;

        let status = Command::new(ffmpeg)
            .args([
                "-hide_banner",
                "-loglevel", "error",
                "-y",
                "-i", src_arg,
                "-c", "copy",
                "-movflags", "+faststart",
                "-f", "mp4",
                dst_arg,
            ])
            .status()
            .map_err(|e| CoreError::Recorder(format!("failed to spawn ffmpeg for remux: {e}")))?;
        if !status.success() {
            // Don't leave a half-written .mp4 around if ffmpeg failed.
            let _ = std::fs::remove_file(dst);
            return Err(CoreError::Recorder(format!(
                "ffmpeg remux failed with status {status} ({} → {})",
                src.display(),
                dst.display()
            )));
        }
        Ok(())
    }
}
