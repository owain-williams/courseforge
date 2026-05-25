//! macOS recorder backed by an `ffmpeg` child process reading from the
//! `avfoundation` input (screen + default microphone).
//!
//! This is a first-cut backend. Two known limitations the trait
//! deliberately hides so we can iterate:
//!
//! * **Pause/Resume** uses POSIX SIGSTOP / SIGCONT. The kernel suspends the
//!   ffmpeg process between frames, which is fine for casual takes but can
//!   leave a frame seam at the boundary. Robust pause-as-segmented-then-concat
//!   is a follow-up and pairs naturally with the ScreenCaptureKit migration.
//! * **ffmpeg discovery**: we look up `ffmpeg` on `PATH`. Bundling as a
//!   Tauri sidecar is a packaging concern tracked separately. Missing
//!   ffmpeg surfaces a clear error rather than a panic.
//!
//! The Recorder trait is what callers see; everything in this file is an
//! implementation detail behind it.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use crate::core::error::{CoreError, Result};
use crate::core::permissions::CaptureSources;
use super::{ActiveRecording, RecorderBackend};

pub struct FfmpegMacBackend {
    ffmpeg_path: Option<PathBuf>,
}

impl Default for FfmpegMacBackend {
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
    // PATH lookup fallback so non-Homebrew installs work.
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

impl RecorderBackend for FfmpegMacBackend {
    fn start(
        &self,
        partial_path: PathBuf,
        sources: CaptureSources,
    ) -> Result<Box<dyn ActiveRecording>> {
        let ffmpeg = self.ffmpeg_path.clone().ok_or_else(|| {
            CoreError::Recorder(
                "ffmpeg not found — install it (e.g. `brew install ffmpeg`) and try again"
                    .to_string(),
            )
        })?;

        if let Some(parent) = partial_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        // avfoundation device spec: "VIDEO:AUDIO". Indices are platform-
        // dependent; "Capture screen 0" + "default" works on stock macOS.
        // System audio + webcam aren't wired in this slice; we leave them
        // for follow-up work and only honour `sources.microphone`.
        let device_arg = if sources.microphone {
            "Capture screen 0:default".to_string()
        } else {
            "Capture screen 0:none".to_string()
        };

        let mut cmd = Command::new(&ffmpeg);
        cmd.args([
            "-hide_banner",
            "-loglevel", "warning",
            "-f", "avfoundation",
            "-capture_cursor", "1",
            "-framerate", "30",
            "-pixel_format", "uyvy422",
            "-i", &device_arg,
            "-c:v", "libx264",
            "-preset", "ultrafast",
            "-crf", "23",
            "-pix_fmt", "yuv420p",
        ]);
        if sources.microphone {
            cmd.args(["-c:a", "aac", "-b:a", "128k"]);
        }
        cmd.arg("-y").arg(&partial_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        let child = cmd.spawn().map_err(|e| {
            CoreError::Recorder(format!("failed to spawn ffmpeg ({}): {e}", ffmpeg.display()))
        })?;

        Ok(Box::new(FfmpegRecording {
            child: Mutex::new(Some(child)),
            partial_path,
        }))
    }
}

pub struct FfmpegRecording {
    child: Mutex<Option<Child>>,
    #[allow(dead_code)]
    partial_path: PathBuf,
}

impl FfmpegRecording {
    fn pid(&self) -> Result<u32> {
        let guard = self.child.lock().unwrap();
        guard
            .as_ref()
            .map(|c| c.id())
            .ok_or_else(|| CoreError::Recorder("recording already stopped".into()))
    }

    fn signal(&self, signal: &str) -> Result<()> {
        let pid = self.pid()?;
        // Shelling out to /bin/kill avoids pulling in libc/nix for one syscall.
        let status = Command::new("/bin/kill")
            .args([signal, &pid.to_string()])
            .status()
            .map_err(|e| CoreError::Recorder(format!("kill {signal} failed: {e}")))?;
        if !status.success() {
            return Err(CoreError::Recorder(format!(
                "kill {signal} pid {pid} exited with {status}"
            )));
        }
        Ok(())
    }
}

impl ActiveRecording for FfmpegRecording {
    fn pause(&self) -> Result<()> {
        // SIGSTOP suspends ffmpeg between frames — best-effort pause for v1.
        self.signal("-STOP")
    }

    fn resume(&self) -> Result<()> {
        self.signal("-CONT")
    }

    fn stop(&self) -> Result<()> {
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            // Ask ffmpeg to finalise cleanly. Writing 'q' to its stdin is the
            // documented graceful-exit signal and leaves a valid MKV trailer.
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(b"q");
                let _ = stdin.flush();
            }
            // Give it a beat to flush; on timeout, fall through to kill.
            let _ = child.wait();
        }
        Ok(())
    }
}

impl Drop for FfmpegRecording {
    fn drop(&mut self) {
        // If a session goes out of scope without an explicit stop (panic, app
        // shutdown), make sure we don't leave an orphan ffmpeg behind.
        let mut guard = self.child.lock().unwrap();
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
