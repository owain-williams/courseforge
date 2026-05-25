//! Container remuxing: turn a finalised `.partial.mkv` capture into the
//! `.mp4` we hand to the WebKit `<video>` element.
//!
//! WebKit on macOS (the engine Tauri uses) does **not** support Matroska
//! containers in HTML5 `<video>` — only MP4 / MOV / M4V / WebM. The codecs
//! we capture with (H.264 + AAC) are fine; only the container needs to
//! change. Doing this as a stream-copy remux (no re-encode) takes well
//! under a second per Segment and is lossless.
//!
//! The capture step keeps writing `.partial.mkv` (Matroska tolerates abrupt
//! termination better than MP4, which needs a clean `moov` atom), so the
//! crash-recovery story for in-progress recordings is unchanged. Only the
//! finalised file changes container.

use std::path::Path;
use crate::core::error::Result;

pub mod fake;

#[cfg(target_os = "macos")]
pub mod ffmpeg_mac;

/// A `Remuxer` knows how to losslessly rewrap the audio + video streams from
/// `src` into an MP4 at `dst`. Implementations don't transcode — they just
/// change the container.
pub trait Remuxer: Send + Sync {
    fn remux_to_mp4(&self, src: &Path, dst: &Path) -> Result<()>;
}

/// Pick a remuxer for the current platform. macOS gets the ffmpeg-backed
/// impl; everywhere else (and headless test harnesses) falls back to the
/// fake so the rest of the app still links.
pub fn default_remuxer() -> Box<dyn Remuxer> {
    #[cfg(target_os = "macos")]
    {
        Box::new(ffmpeg_mac::FfmpegMacRemuxer::default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(fake::FakeRemuxer::default())
    }
}
