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
use std::time::{Duration, Instant};

use crate::core::error::{CoreError, Result};
use crate::core::permissions::CaptureSources;
use super::{ActiveRecording, RecorderBackend};

/// How long to wait for ffmpeg to finalise the output file after asking
/// nicely (SIGINT) before escalating. 5 seconds is comfortable for a
/// short clip — a longer recording finishing writing a few extra
/// megabytes shouldn't need more than this.
const STOP_GRACEFUL_TIMEOUT: Duration = Duration::from_secs(5);

/// After SIGTERM, how long before we hard-kill. SIGTERM gives ffmpeg
/// another chance to clean up; if it's still stuck after this, the file
/// is likely already lost so we move on.
const STOP_TERM_TIMEOUT: Duration = Duration::from_secs(2);

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
            // No `-pixel_format` for the input: forcing one (e.g. `uyvy422`)
            // makes avfoundation refuse the configuration on Retina displays
            // because the requested format × resolution × framerate exceeds
            // what the screen capture device can deliver. The symptom was
            // "Configuration of video device failed, falling back to
            // default" plus an ffmpeg that ran forever without writing any
            // frames to the output file. Letting avfoundation negotiate its
            // native format (nv12 / uyvy422 depending on hardware) and
            // letting swscale convert to `-pix_fmt yuv420p` for libx264
            // works on every Mac we've tested.
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
        send_signal(pid, signal)
    }
}

/// Shell out to `/bin/kill` to send a signal by pid. Pulled out as a free
/// function so `stop()` can call it after taking ownership of the `Child`
/// (which makes `self.pid()` unavailable).
fn send_signal(pid: u32, signal: &str) -> Result<()> {
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

/// Poll `try_wait` until the child exits or `timeout` elapses. Returns
/// `true` iff the child exited within the budget. We poll because the
/// stdlib's `Child::wait` is unbounded and there's no `wait_timeout` in
/// std (the crate of that name would add a dep for one syscall).
fn wait_with_timeout(child: &mut Child, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            // try_wait erroring usually means the process is already
            // reaped — treat as exited so we don't loop forever.
            Err(_) => return true,
        }
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
        let mut child = match self.child.lock().unwrap().take() {
            Some(c) => c,
            None => return Ok(()),
        };
        let pid = child.id();

        // 1. SIGCONT — defensive. If the user paused (SIGSTOP) then went
        //    straight to Stop, ffmpeg can't act on any later signal until
        //    it's resumed. SIGCONT on an already-running process is a no-op.
        let _ = send_signal(pid, "-CONT");

        // 2. SIGINT — the documented graceful shutdown for ffmpeg. Far more
        //    reliable than writing `q` to stdin: with `-i avfoundation` the
        //    main loop is busy draining the capture device and rarely polls
        //    stdin promptly, which was hanging Stop indefinitely.
        let _ = send_signal(pid, "-INT");
        // Drop stdin so a still-active polling read sees EOF too.
        drop(child.stdin.take());

        if wait_with_timeout(&mut child, STOP_GRACEFUL_TIMEOUT) {
            return Ok(());
        }

        // 3. Escalate to SIGTERM. ffmpeg may not finalise the file cleanly
        //    from here, but discard+re-record is a survivable fallback.
        let _ = send_signal(pid, "-TERM");
        if wait_with_timeout(&mut child, STOP_TERM_TIMEOUT) {
            return Ok(());
        }

        // 4. Last resort — guarantees the lock-holding caller eventually
        //    returns rather than wedging the UI on a stuck ffmpeg.
        let _ = child.kill();
        let _ = child.wait();
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
