//! macOS exporter backed by `ffmpeg` + `ffprobe`.
//!
//! Renders a Video → MP4 by running ffmpeg once with a `trim` / `atrim`
//! filter graph for every kept range, then concatenating the resulting
//! sub-streams in order via the `concat` filter. We re-encode H.264 +
//! AAC (you can't stream-copy after a filter graph) — slower than copy
//! but the AC explicitly calls out "no audio/video desync", and trim +
//! concat is the standard ffmpeg incantation that guarantees clean PTS
//! at every cut boundary. `setpts=PTS-STARTPTS` per sub-stream is what
//! resets each one back to a 0-based PTS so concat can stitch them.
//!
//! Progress is parsed from ffmpeg's machine-readable `-progress pipe:1`
//! stream. Cancellation kills the child process and removes any partial
//! `dst` so the manager's contract ("no half-written output on cancel")
//! holds at this level too.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::core::error::{CoreError, Result};

use super::{CancelToken, Exporter, ProgressSink};

/// How often the worker thread polls the cancel flag while reading
/// progress. Short enough that "I clicked Cancel and nothing happened" is
/// not a real user complaint; long enough that we're not burning a core.
const CANCEL_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub struct FfmpegMacExporter {
    ffmpeg_path: Option<PathBuf>,
    ffprobe_path: Option<PathBuf>,
}

impl Default for FfmpegMacExporter {
    fn default() -> Self {
        Self {
            ffmpeg_path: which("ffmpeg"),
            ffprobe_path: which("ffprobe"),
        }
    }
}

fn which(binary: &str) -> Option<PathBuf> {
    let candidates = [
        format!("/opt/homebrew/bin/{binary}"),
        format!("/usr/local/bin/{binary}"),
        format!("/usr/bin/{binary}"),
    ];
    for c in candidates {
        let p = PathBuf::from(c);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Ok(out) = Command::new("/usr/bin/which").arg(binary).output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(PathBuf::from(s));
            }
        }
    }
    None
}

/// Read a media file's playable duration in seconds via `ffprobe`. Returns
/// `None` if ffprobe isn't installed or the probe failed — callers decide
/// whether that's fatal.
pub fn probe_duration_sec(path: &Path) -> Option<f64> {
    let ffprobe = which("ffprobe")?;
    let out = Command::new(ffprobe)
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    s.parse::<f64>().ok().filter(|d| d.is_finite() && *d > 0.0)
}

impl Exporter for FfmpegMacExporter {
    fn export_mp4(
        &self,
        src: &Path,
        keep_ranges: &[(f64, f64)],
        dst: &Path,
        progress: &dyn ProgressSink,
        cancel: &CancelToken,
    ) -> Result<()> {
        let ffmpeg = self.ffmpeg_path.as_ref().ok_or_else(|| {
            CoreError::Exporter(
                "ffmpeg not found — install it (e.g. `brew install ffmpeg`) and try again".into(),
            )
        })?;
        let _ = &self.ffprobe_path;

        if keep_ranges.is_empty() {
            return Err(CoreError::Exporter(
                "every part of this Video is inside a cut — nothing to export".into(),
            ));
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        let total_kept: f64 = keep_ranges.iter().map(|(s, e)| e - s).sum();
        let filter_complex = build_trim_concat_filter(keep_ranges);

        let src_arg = src.to_str().ok_or_else(|| {
            CoreError::Exporter(format!("source path is not valid utf-8: {}", src.display()))
        })?;
        let dst_arg = dst.to_str().ok_or_else(|| {
            CoreError::Exporter(format!("dest path is not valid utf-8: {}", dst.display()))
        })?;

        // `-filter_complex` with one `trim`/`atrim` pair per kept range,
        // each followed by `setpts=PTS-STARTPTS` / `asetpts=PTS-STARTPTS`
        // so the sub-streams start at PTS 0. Then a single `concat`
        // stitches them into the output. Re-encoding (libx264 / aac) is
        // required because filter graphs operate on decoded frames.
        // `-movflags +faststart` moves the MP4 moov atom to the head so
        // QuickTime / VLC can begin playback before reading the whole
        // file.
        let mut cmd = Command::new(ffmpeg);
        cmd.args([
            "-hide_banner",
            "-nostdin",
            "-loglevel", "error",
            "-y",
            "-i", src_arg,
            "-filter_complex", &filter_complex,
            "-map", "[outv]",
            "-map", "[outa]",
            "-c:v", "libx264",
            "-preset", "veryfast",
            "-crf", "20",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac",
            "-b:a", "192k",
            "-movflags", "+faststart",
            "-f", "mp4",
            "-progress", "pipe:1",
            "-nostats",
            dst_arg,
        ]);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            CoreError::Exporter(format!("failed to spawn ffmpeg: {e}"))
        })?;

        let stdout = child
            .stdout
            .take()
            .expect("we asked for piped stdout above");
        let reader = BufReader::new(stdout);
        let child = Mutex::new(Some(child));

        let drive = pump_progress(reader, total_kept, progress, cancel, &child);

        let mut guard = child.lock().unwrap();
        let mut child = guard.take().expect("child must still be present");

        match drive {
            Ok(()) => {
                // ffmpeg may still be flushing the muxer; wait for it.
                let status = child.wait().map_err(|e| CoreError::Exporter(
                    format!("waiting on ffmpeg failed: {e}"),
                ))?;
                if !status.success() {
                    let stderr = read_stderr(&mut child);
                    let _ = std::fs::remove_file(dst);
                    return Err(CoreError::Exporter(format!(
                        "ffmpeg exited with {status}: {stderr}"
                    )));
                }
                progress.report(1.0);
                Ok(())
            }
            Err(CoreError::ExportCancelled) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(dst);
                Err(CoreError::ExportCancelled)
            }
            Err(other) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_file(dst);
                Err(other)
            }
        }
    }
}

fn pump_progress<R: BufRead>(
    mut reader: R,
    total_kept: f64,
    progress: &dyn ProgressSink,
    cancel: &CancelToken,
    child: &Mutex<Option<Child>>,
) -> Result<()> {
    progress.report(0.0);
    let mut buf = String::new();
    loop {
        if cancel.is_cancelled() {
            if let Some(mut c) = child.lock().unwrap().take() {
                let _ = c.kill();
                // Re-insert so the outer code can `.wait()` and surface
                // post-kill status uniformly.
                *child.lock().unwrap() = Some(c);
            }
            return Err(CoreError::ExportCancelled);
        }
        buf.clear();
        let n = reader.read_line(&mut buf).map_err(|e| {
            CoreError::Exporter(format!("reading ffmpeg progress failed: {e}"))
        })?;
        if n == 0 {
            // EOF — ffmpeg closed the pipe.
            return Ok(());
        }
        // ffmpeg's progress output is `key=value` lines. We care about
        // `out_time_ms` (microseconds, despite the name) and the terminal
        // `progress=end` marker.
        let line = buf.trim();
        if let Some(value) = line.strip_prefix("out_time_us=") {
            if let Ok(us) = value.parse::<u64>() {
                if total_kept > 0.0 {
                    let secs = us as f64 / 1_000_000.0;
                    let frac = (secs / total_kept).clamp(0.0, 0.999);
                    progress.report(frac);
                }
            }
        } else if let Some(value) = line.strip_prefix("out_time_ms=") {
            // Older ffmpeg releases used `_ms` for the same microsecond value.
            if let Ok(us) = value.parse::<u64>() {
                if total_kept > 0.0 {
                    let secs = us as f64 / 1_000_000.0;
                    let frac = (secs / total_kept).clamp(0.0, 0.999);
                    progress.report(frac);
                }
            }
        } else if line == "progress=end" {
            return Ok(());
        }
        // Tiny sleep so the cancel poll doesn't pin a core on the unusual
        // case where ffmpeg is silent for a stretch (e.g. between very
        // large frames).
        std::thread::sleep(CANCEL_POLL_INTERVAL / 10);
    }
}

fn read_stderr(child: &mut Child) -> String {
    let Some(mut stderr) = child.stderr.take() else { return String::new() };
    use std::io::Read;
    let mut out = String::new();
    let _ = stderr.read_to_string(&mut out);
    out.lines().rev().take(20).collect::<Vec<_>>().join("\n")
}

/// Build the `-filter_complex` graph for an ordered list of keep ranges.
///
/// Output graph shape (for N ranges):
/// ```text
/// [0:v]trim=S1:E1,setpts=PTS-STARTPTS[v0];
/// [0:a]atrim=S1:E1,asetpts=PTS-STARTPTS[a0];
/// ...
/// [v0][a0][v1][a1]...[vN-1][aN-1]concat=n=N:v=1:a=1[outv][outa]
/// ```
///
/// The `concat` filter expects video / audio pairs interleaved in the
/// order `[v0][a0][v1][a1]...`. The `n=N:v=1:a=1` says there are N
/// segments, each with 1 video and 1 audio stream.
fn build_trim_concat_filter(ranges: &[(f64, f64)]) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (i, (s, e)) in ranges.iter().enumerate() {
        parts.push(format!(
            "[0:v]trim=start={s:.6}:end={e:.6},setpts=PTS-STARTPTS[v{i}]"
        ));
        parts.push(format!(
            "[0:a]atrim=start={s:.6}:end={e:.6},asetpts=PTS-STARTPTS[a{i}]"
        ));
    }
    let concat_inputs: String = (0..ranges.len())
        .map(|i| format!("[v{i}][a{i}]"))
        .collect();
    parts.push(format!(
        "{concat_inputs}concat=n={}:v=1:a=1[outv][outa]",
        ranges.len()
    ));
    parts.join(";")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_trim_concat_filter_for_one_range_has_one_trim_and_concat_n1() {
        let f = build_trim_concat_filter(&[(0.0, 10.0)]);
        assert!(f.contains("trim=start=0.000000:end=10.000000"));
        assert!(f.contains("atrim=start=0.000000:end=10.000000"));
        assert!(f.contains("concat=n=1:v=1:a=1[outv][outa]"));
    }

    #[test]
    fn build_trim_concat_filter_for_multiple_ranges_chains_them_in_order() {
        let f = build_trim_concat_filter(&[(0.0, 2.0), (4.0, 7.0), (8.0, 10.0)]);
        // Every range gets its own trim/atrim.
        assert_eq!(f.matches("trim=start=").count() - f.matches("atrim=start=").count(), 3);
        // Concat input order is v0,a0,v1,a1,v2,a2 → concat=n=3.
        assert!(f.contains("[v0][a0][v1][a1][v2][a2]concat=n=3:v=1:a=1[outv][outa]"));
    }
}
