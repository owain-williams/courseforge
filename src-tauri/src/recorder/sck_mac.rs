//! macOS in-process recorder backed by ScreenCaptureKit, writing a single
//! `.mov` per Take via SCK's built-in `SCRecordingOutput` (which uses
//! `AVAssetWriter` under the hood — same on-disk artifact as a hand-rolled
//! writer, no exposed pixel-buffer plumbing).
//!
//! Phase 1 scope (per ADR-0002 and issue #30):
//!
//! * **Inputs**: one `Screen` capture request, plus optionally one
//!   `Microphone` request — bundled into the same `.mov` via SCK's
//!   `captureMicrophone` config (macOS 15+). v1 behaviour parity.
//! * **Other source roles**: `Window`, `Camera`, `SystemAudio` are rejected
//!   at Start with a per-request diagnostic — Phase 2 wires them up.
//! * **Pause / Resume**: implemented as a no-op in Phase 1. SCK's
//!   `SCRecordingOutput` writes via the OS's coalesced pipeline and doesn't
//!   expose a frame-accurate pause hook. The state machine still flips, the
//!   `.mov` stays a continuous recording, and there are no PTS regressions.
//!   Phase 2 (which moves to per-source `AVAssetWriter` feeds with a
//!   manual `SCStreamOutput` delegate) will add the sample-drop gate that
//!   makes pause/resume frame-accurate.
//! * **Atomic Start**: all setup (display enumeration, content filter,
//!   configuration, recording output, capture start) runs on the calling
//!   thread synchronously. Any failure tears the partial pipeline down
//!   before the function returns, so the registry never sees a session
//!   for a recording that didn't actually start.
//! * **Crash isolation**: every objc2 boundary is wrapped in
//!   `std::panic::catch_unwind` so a binding-level surprise can't take
//!   down the Tauri app.
//!
//! Tactical / HITL decisions made here (the four called out in issue #30):
//!
//! * **SCStream configuration** — main display, full content, default
//!   pixel format, 30 fps minimum frame interval (`captureMicrophone =
//!   true` when a mic request is present).
//! * **CMSampleBuffer → AVAssetWriter adaptor pattern** — *not used in
//!   Phase 1*. SCK's `SCRecordingOutput` writes directly to disk, so the
//!   adaptor lives inside SCK rather than in our code. Phase 2's
//!   multi-source path swaps in our own `SCStreamOutput` delegate with a
//!   per-source `AVAssetWriterInput`.
//! * **Pause / Resume under AssetWriter semantics** — no-op pause in
//!   Phase 1 (see above); the Phase 2 plan is sample-drop gating before
//!   the writer.append call.
//! * **In-process crash isolation strategy** — every entry into the
//!   binding layer goes through `catch_unwind`; SCK / AssetWriter errors
//!   are mapped to `CoreError::Recorder` with the underlying `NSError`'s
//!   localized description spliced in.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::core::capture::{CaptureRequest, SourceRole};
use crate::core::error::{CoreError, Result};
use super::{ActiveRecording, RecorderBackend};

/// Phase 1 in-process recorder. Per-Take state lives on the `SckMacRecording`
/// handle the backend returns from `start`.
#[derive(Default)]
pub struct SckMacBackend;

impl RecorderBackend for SckMacBackend {
    fn start(
        &self,
        partial_path: PathBuf,
        requests: Vec<CaptureRequest>,
        take_id: String,
    ) -> Result<Box<dyn ActiveRecording>> {
        validate_phase1_requests(&requests)?;
        if let Some(parent) = partial_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
        }

        let want_mic = requests.iter().any(|r| r.role == SourceRole::Microphone);
        let session = catch_panic("SckMacBackend::start", || {
            imp::Session::start(&partial_path, want_mic, &take_id)
        })?;
        Ok(Box::new(SckMacRecording {
            partial_path,
            session: Mutex::new(Some(session)),
        }))
    }
}

pub struct SckMacRecording {
    #[allow(dead_code)] // surfaced via diagnostics in future revisions
    partial_path: PathBuf,
    session: Mutex<Option<imp::Session>>,
}

impl ActiveRecording for SckMacRecording {
    fn pause(&self) -> Result<()> {
        // Phase 1: no-op (see module doc). Pause's visible behaviour is
        // that the session state flips; the recorded `.mov` is one
        // continuous capture from Start to Stop.
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        // Phase 1: no-op. See `pause`.
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let session = self.session.lock().unwrap().take();
        if let Some(session) = session {
            catch_panic("SckMacBackend::stop", move || session.stop())?;
        }
        Ok(())
    }
}

impl Drop for SckMacRecording {
    fn drop(&mut self) {
        // Best-effort tear-down if the manager dropped us without calling
        // stop (panic, app shutdown). Don't propagate errors from Drop.
        if let Some(session) = self.session.lock().unwrap().take() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.stop()));
        }
    }
}

/// Wrap an objc2-touching block in `catch_unwind` so a panic on the
/// binding side becomes a `CoreError::Recorder` rather than aborting the
/// Tauri app. The label is folded into the error so panics from different
/// call sites are distinguishable in logs.
///
/// We assert unwind-safety on the closure because objc2 retained handles
/// don't implement `UnwindSafe` (they contain `UnsafeCell`s for objc
/// runtime reasons). Our usage is straight-line: if a binding call
/// panics, we drop everything below and return; no state inside the
/// SCK objects needs to be observed after the panic.
fn catch_panic<T>(
    label: &'static str,
    f: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let unwind_safe = std::panic::AssertUnwindSafe(f);
    match std::panic::catch_unwind(unwind_safe) {
        Ok(r) => r,
        Err(payload) => {
            let msg = panic_payload_to_string(payload);
            Err(CoreError::Recorder(format!(
                "panic in objc2 binding at {label}: {msg}"
            )))
        }
    }
}

fn panic_payload_to_string(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        s.to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Phase 1 only handles a single Screen source plus optional Microphone.
/// Anything else is rejected up front with a per-request diagnostic so the
/// UI can point the user at the unsupported entry.
fn validate_phase1_requests(requests: &[CaptureRequest]) -> Result<()> {
    let mut screens = 0;
    let mut mics = 0;
    for r in requests {
        match r.role {
            SourceRole::Screen => screens += 1,
            SourceRole::Microphone => mics += 1,
            other => {
                return Err(CoreError::Recorder(format!(
                    "Phase 1 recorder cannot capture {:?} sources yet — Phase 2 adds Window, Camera, and SystemAudio. Device requested: {} ({}).",
                    other, r.device.label, r.device.id,
                )));
            }
        }
    }
    if screens != 1 {
        return Err(CoreError::Recorder(format!(
            "Phase 1 recorder needs exactly one Screen source (got {screens})"
        )));
    }
    if mics > 1 {
        return Err(CoreError::Recorder(format!(
            "Phase 1 recorder supports at most one Microphone source (got {mics})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CompositionDefaults, Device};

    fn req(role: SourceRole) -> CaptureRequest {
        CaptureRequest {
            role,
            device: Device {
                id: "default".into(),
                label: "Default".into(),
            },
            defaults: CompositionDefaults::default(),
        }
    }

    #[test]
    fn validate_accepts_screen_only() {
        validate_phase1_requests(&[req(SourceRole::Screen)]).unwrap();
    }

    #[test]
    fn validate_accepts_screen_plus_microphone() {
        validate_phase1_requests(&[req(SourceRole::Screen), req(SourceRole::Microphone)])
            .unwrap();
    }

    #[test]
    fn validate_rejects_window_source_in_phase1() {
        let err = validate_phase1_requests(&[req(SourceRole::Window)]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Window") || msg.contains("Screen"), "msg was {msg}");
    }

    #[test]
    fn validate_rejects_camera_source_in_phase1() {
        let err = validate_phase1_requests(&[req(SourceRole::Screen), req(SourceRole::Camera)])
            .unwrap_err();
        assert!(err.to_string().contains("Camera"));
    }

    #[test]
    fn validate_rejects_two_screens() {
        let err = validate_phase1_requests(&[req(SourceRole::Screen), req(SourceRole::Screen)])
            .unwrap_err();
        assert!(err.to_string().contains("Screen"));
    }

    #[test]
    fn validate_rejects_two_microphones() {
        let err = validate_phase1_requests(&[
            req(SourceRole::Screen),
            req(SourceRole::Microphone),
            req(SourceRole::Microphone),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("Microphone"));
    }
}

// ---------------------------------------------------------------------------
// SCK wire-up. All `unsafe` lives in this submodule so the outer Backend /
// ActiveRecording shape stays readable and the audit surface is contained.
// ---------------------------------------------------------------------------

mod imp {
    use std::path::Path;
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{define_class, msg_send, AllocAnyThread};
    use objc2_av_foundation::AVFileTypeQuickTimeMovie;
    use objc2_core_media::CMTime;
    use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURL};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCDisplay, SCRecordingOutput, SCRecordingOutputConfiguration,
        SCRecordingOutputDelegate, SCShareableContent, SCStream, SCStreamConfiguration,
    };

    use crate::core::error::{CoreError, Result};

    /// How long we'll wait for SCK's async APIs (display enumeration, start
    /// capture, stop capture) to call their completion handlers. Generous
    /// because cold-starting SCK on a Mac that's been idle can take a
    /// surprising moment, but not so long that the UI hangs forever on a
    /// genuinely-broken capture.
    const SCK_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

    /// Live Phase-1 Take. Holds retained handles to every SCK object that
    /// needs to outlive Start, so they aren't released out from under the
    /// recording. Stop drops the lot in the right order.
    pub struct Session {
        stream: Retained<SCStream>,
        recording_output: Retained<SCRecordingOutput>,
        // Keep the delegate alive for the lifetime of the recording —
        // SCRecordingOutput holds it only weakly via the protocol object.
        _delegate: Retained<RecordingDelegate>,
    }

    // SCK's documented thread-safety: SCStream, SCRecordingOutput, and our
    // empty-state RecordingDelegate are all safe to use from arbitrary
    // threads. The Mutex in the outer SckMacRecording serialises calls
    // into here, so we never actually share these handles between threads
    // simultaneously — Send is enough; Sync we assert defensively so the
    // type can sit in a Mutex without an extra wrapper.
    unsafe impl Send for Session {}
    unsafe impl Sync for Session {}

    impl Session {
        pub fn start(partial_path: &Path, want_mic: bool, _take_id: &str) -> Result<Self> {
            let path_str = partial_path
                .to_str()
                .ok_or_else(|| CoreError::Recorder(format!(
                    "partial path is not valid UTF-8: {}",
                    partial_path.display()
                )))?;

            // Async-shaped APIs (SCShareableContent.getShareableContent…,
            // SCStream.start/stopCapture…) callback to us via `block2`
            // blocks. We bridge to a synchronous Start via a mpsc channel
            // — same pattern in three places below.

            // ----- 1. Enumerate shareable content (displays). ---------------
            let content = await_shareable_content()?;
            let displays = unsafe { content.displays() };
            if displays.is_empty() {
                return Err(CoreError::Recorder(
                    "no displays available to capture (SCShareableContent returned an empty display list)".into(),
                ));
            }
            // Phase 1 captures the first / main display. Phase 2's Scene
            // editor lets the user pick.
            let display: Retained<SCDisplay> = displays.objectAtIndex(0);

            // ----- 2. Build content filter for the whole display. -----------
            // initWithDisplay:excludingWindows: with an empty exclusion
            // list captures everything on that display.
            let empty_windows: Retained<objc2_foundation::NSArray<objc2_screen_capture_kit::SCWindow>> =
                objc2_foundation::NSArray::new();
            let filter = unsafe {
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &empty_windows,
                )
            };

            // ----- 3. Stream configuration. --------------------------------
            let config: Retained<SCStreamConfiguration> = unsafe { SCStreamConfiguration::new() };
            unsafe {
                // 30 fps minimum frame interval ≈ 1/30 second. CMTime is
                // (value, timescale) with timescale 600 a common pick that
                // divides cleanly by 30 / 60.
                config.setMinimumFrameInterval(CMTime {
                    value: 20,
                    timescale: 600,
                    flags: objc2_core_media::CMTimeFlags::Valid,
                    epoch: 0,
                });
                if want_mic {
                    config.setCapturesAudio(true);
                    config.setCaptureMicrophone(true);
                }
            }

            // ----- 4. Stream. ---------------------------------------------
            let stream = unsafe {
                SCStream::initWithFilter_configuration_delegate(
                    SCStream::alloc(),
                    &filter,
                    &config,
                    None,
                )
            };

            // ----- 5. Recording output: writes screen + mic to one .mov. --
            let rec_config: Retained<SCRecordingOutputConfiguration> =
                unsafe { SCRecordingOutputConfiguration::new() };
            let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));
            unsafe {
                rec_config.setOutputURL(&url);
                let file_type = AVFileTypeQuickTimeMovie
                    .expect("AVFileTypeQuickTimeMovie unavailable — AVFoundation not linked?");
                rec_config.setOutputFileType(file_type);
            }

            let delegate: Retained<RecordingDelegate> = unsafe {
                let alloced = RecordingDelegate::alloc().set_ivars(());
                msg_send![super(alloced), init]
            };
            let delegate_proto: &ProtocolObject<dyn SCRecordingOutputDelegate> =
                ProtocolObject::from_ref(&*delegate);

            let recording_output = unsafe {
                SCRecordingOutput::initWithConfiguration_delegate(
                    SCRecordingOutput::alloc(),
                    &rec_config,
                    delegate_proto,
                )
            };

            // Add output before starting capture so the first sample lands
            // in the recording file (per Apple's documented contract).
            unsafe {
                stream
                    .addRecordingOutput_error(&recording_output)
                    .map_err(|e| nserror_to_recorder("addRecordingOutput", &e))?;
            }

            // ----- 6. Start capture (async, waits for completion). --------
            await_capture_start(&stream)?;

            Ok(Self {
                stream,
                recording_output,
                _delegate: delegate,
            })
        }

        pub fn stop(self) -> Result<()> {
            // Remove the recording output first so SCK can finalise the
            // file deterministically (per Apple's docs, this guarantees
            // recordingOutputDidFinishRecording fires before stop returns).
            unsafe {
                self.stream
                    .removeRecordingOutput_error(&self.recording_output)
                    .map_err(|e| nserror_to_recorder("removeRecordingOutput", &e))?;
            }
            await_capture_stop(&self.stream)?;
            // `stream`, `recording_output`, `_delegate` drop here — their
            // retained references are the last ones, so SCK releases the
            // underlying objects.
            Ok(())
        }
    }

    // ----- helpers ---------------------------------------------------------

    fn await_shareable_content() -> Result<Retained<SCShareableContent>> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new({
            let tx = tx.clone();
            move |content: *mut SCShareableContent, error: *mut NSError| {
                let result: Result<Retained<SCShareableContent>> = if !error.is_null() {
                    let err = unsafe { &*error };
                    Err(nserror_to_recorder("getShareableContent", err))
                } else if content.is_null() {
                    Err(CoreError::Recorder(
                        "SCShareableContent completion handler returned no content".into(),
                    ))
                } else {
                    let retained = unsafe { Retained::retain(content) }
                        .ok_or_else(|| CoreError::Recorder(
                            "SCShareableContent: failed to retain content pointer".into(),
                        ));
                    retained
                };
                let _ = tx.send(result);
            }
        });
        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&handler);
        }
        match rx.recv_timeout(SCK_CALLBACK_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err(CoreError::Recorder(
                "SCShareableContent enumeration timed out — Screen Recording permission missing?".into(),
            )),
        }
    }

    fn await_capture_start(stream: &SCStream) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new({
            let tx = tx.clone();
            move |error: *mut NSError| {
                let result: Result<()> = if !error.is_null() {
                    let err = unsafe { &*error };
                    Err(nserror_to_recorder("startCapture", err))
                } else {
                    Ok(())
                };
                let _ = tx.send(result);
            }
        });
        unsafe {
            stream.startCaptureWithCompletionHandler(Some(&handler));
        }
        match rx.recv_timeout(SCK_CALLBACK_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err(CoreError::Recorder(
                "SCStream.startCapture timed out — check Screen Recording permission".into(),
            )),
        }
    }

    fn await_capture_stop(stream: &SCStream) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new({
            let tx = tx.clone();
            move |error: *mut NSError| {
                let result: Result<()> = if !error.is_null() {
                    let err = unsafe { &*error };
                    Err(nserror_to_recorder("stopCapture", err))
                } else {
                    Ok(())
                };
                let _ = tx.send(result);
            }
        });
        unsafe {
            stream.stopCaptureWithCompletionHandler(Some(&handler));
        }
        match rx.recv_timeout(SCK_CALLBACK_TIMEOUT) {
            Ok(r) => r,
            // Don't error on stop timeout — once we've asked SCK to stop,
            // the file is whatever it is on disk; we'd rather return Ok
            // and let `finalize_segment`'s file checks be the source of
            // truth than block the UI.
            Err(_) => Ok(()),
        }
    }

    fn nserror_to_recorder(context: &str, error: &NSError) -> CoreError {
        let localized = error.localizedDescription();
        let code: i64 = unsafe { msg_send![error, code] };
        CoreError::Recorder(format!(
            "{context} failed: {} (code {})",
            localized.to_string(),
            code
        ))
    }

    // ----- delegate -------------------------------------------------------
    //
    // SCRecordingOutput requires a delegate (the initWithConfiguration:delegate:
    // signature is non-nullable). All three methods on the protocol are
    // `#[optional]` though — they're informational notifications about
    // recording lifecycle. Phase 1 has nothing to do with them; the
    // session's mpsc channels handle synchronous coordination instead.

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "CourseforgeRecordingDelegate"]
        #[ivars = ()]
        struct RecordingDelegate;

        unsafe impl NSObjectProtocol for RecordingDelegate {}

        unsafe impl SCRecordingOutputDelegate for RecordingDelegate {}
    );
}
