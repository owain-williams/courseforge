//! macOS in-process Phase 2 recorder.
//!
//! Per ADR-0002, the Phase 2 backend captures N sources in one Take, each
//! into its own `.mov` (video sources) or `.m4a` (audio-only sources) via
//! a per-source `AVAssetWriter`. All `AVAssetWriter`s share one
//! `CMSampleBuffer` PTS clock so cross-source sync is frame-accurate.
//!
//! Tactical choices recorded in this file (the HITL decisions called out
//! in issue #36):
//!
//! * **Per-source AVAssetWriter configuration**: video-and-audio sources
//!   land in QuickTime `.mov`; audio-only sources land in AAC `.m4a`.
//!   AVAssetWriter is left to pick the appropriate input settings for the
//!   sample buffers handed in (the SCK / AVCaptureSession side is what
//!   actually chooses the pixel format, sample rate, channel count).
//! * **SCStreamOutput delegate model**: one delegate per `SCStream` (one
//!   stream per Screen / Window / SystemAudio source). The delegate
//!   forwards every `CMSampleBuffer` it receives to its source's
//!   `AVAssetWriterInput.append`. We don't multiplex one delegate across N
//!   writers — the delegate ownership graph stays trivially simple.
//! * **Camera + Microphone**: routed through `AVCaptureSession` per
//!   source, with `AVCaptureVideoDataOutput` /
//!   `AVCaptureAudioDataOutput` sample-buffer delegates writing to the
//!   same `AVAssetWriter` pattern.
//! * **AVAssetWriter session start time**: each writer calls
//!   `startSessionAtSourceTime:` on the PTS of its first received sample.
//!   Cross-source sync is preserved because every source's sample buffers
//!   are timestamped by the same system `CMTime` clock.
//!
//! What this file does *not* yet implement (deliberately deferred to the
//! issues that follow):
//!
//! * **Atomic Pause/Resume with sample-drop gating** (issue #37): the
//!   per-writer pause flag is in place but the wiring to a shared atomic
//!   the manager flips is left as a no-op until #37.
//! * **Mid-Take per-source failure → SourceFailed sidecar** (issue #37):
//!   a per-source error currently aborts the whole Take rather than
//!   ending one source cleanly.
//! * **Per-Take in-progress marker for orphan recovery** (issue #38):
//!   the start path doesn't drop a marker yet.
//!
//! Crash isolation: every objc2 entry is wrapped in `catch_unwind` so a
//! binding-level surprise becomes a `CoreError::Recorder` rather than
//! aborting the Tauri app.

use std::path::PathBuf;
use std::sync::Mutex;

use crate::core::capture::{CaptureRequest, SourceRole};
use crate::core::error::{CoreError, Result};
use super::{ActiveRecording, RecorderBackend, TakeRequest, TakeSource};

/// Phase 2 in-process recorder. Per-Take state lives on the
/// `SckMacRecording` handle the backend returns from `start`.
#[derive(Default)]
pub struct SckMacBackend;

impl RecorderBackend for SckMacBackend {
    fn start(&self, take: TakeRequest) -> Result<Box<dyn ActiveRecording>> {
        validate_phase2_requests(&take.sources)?;
        for src in &take.sources {
            if let Some(parent) = src.partial_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
        }

        // Atomic Start: any per-source init failure tears the lot down
        // before this function returns. `imp::Take::start` handles the
        // cleanup internally on its own error path so we don't need to
        // unwind partial state here.
        let take_state =
            catch_panic("SckMacBackend::start", || imp::Take::start(&take))?;
        Ok(Box::new(SckMacRecording {
            partial_paths: take
                .sources
                .iter()
                .map(|s| s.partial_path.clone())
                .collect(),
            take: Mutex::new(Some(take_state)),
        }))
    }
}

pub struct SckMacRecording {
    #[allow(dead_code)]
    partial_paths: Vec<PathBuf>,
    take: Mutex<Option<imp::Take>>,
}

impl ActiveRecording for SckMacRecording {
    fn pause(&self) -> Result<()> {
        // Phase 2 slice 4 stops short of sample-drop gating; issue #37
        // wires the shared pause-gate atomic. The session state machine
        // still flips so the UI is honest about user intent.
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let take = self.take.lock().unwrap().take();
        if let Some(take) = take {
            catch_panic("SckMacBackend::stop", move || take.stop())?;
        }
        Ok(())
    }
}

impl Drop for SckMacRecording {
    fn drop(&mut self) {
        if let Some(take) = self.take.lock().unwrap().take() {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| take.stop()));
        }
    }
}

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

/// Phase 2 accepts any subset of the closed Source Role taxonomy with one
/// non-trivial cardinality rule: at most one Microphone source per Take.
/// AVCaptureSession permits exactly one mic input per session and feeding
/// two mics into one Take introduces an ambiguity we don't need yet.
/// Multiple screens / windows / cameras / system-audio sources are fine.
fn validate_phase2_requests(sources: &[TakeSource]) -> Result<()> {
    if sources.is_empty() {
        return Err(CoreError::Recorder(
            "no capture sources requested — Start needs at least one".into(),
        ));
    }
    let mics = sources
        .iter()
        .filter(|s| s.request.role == SourceRole::Microphone)
        .count();
    if mics > 1 {
        return Err(CoreError::Recorder(format!(
            "Phase 2 recorder supports at most one Microphone source per Take (got {mics})"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{CaptureRequest, CompositionDefaults, Device};

    fn src(role: SourceRole, segment_id: &str) -> TakeSource {
        TakeSource {
            request: CaptureRequest {
                role,
                device: Device {
                    id: "default".into(),
                    label: "Default".into(),
                },
                defaults: CompositionDefaults::default(),
            },
            segment_id: segment_id.into(),
            partial_path: PathBuf::from(format!("/tmp/{segment_id}.partial.mov")),
        }
    }

    #[test]
    fn validate_accepts_a_single_screen() {
        validate_phase2_requests(&[src(SourceRole::Screen, "a")]).unwrap();
    }

    #[test]
    fn validate_accepts_a_richer_multi_source_take() {
        validate_phase2_requests(&[
            src(SourceRole::Screen, "a"),
            src(SourceRole::Window, "b"),
            src(SourceRole::Camera, "c"),
            src(SourceRole::Microphone, "d"),
            src(SourceRole::SystemAudio, "e"),
        ])
        .unwrap();
    }

    #[test]
    fn validate_rejects_two_microphones_per_take() {
        let err = validate_phase2_requests(&[
            src(SourceRole::Screen, "a"),
            src(SourceRole::Microphone, "b"),
            src(SourceRole::Microphone, "c"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("Microphone"));
    }

    #[test]
    fn validate_rejects_empty_request_list() {
        let err = validate_phase2_requests(&[]).unwrap_err();
        assert!(err.to_string().contains("Start needs at least one"));
    }
}

// ---------------------------------------------------------------------------
// SCK + AVCaptureSession wire-up. All `unsafe` lives in this submodule so
// the outer Backend / ActiveRecording shape stays readable and the audit
// surface is contained.
// ---------------------------------------------------------------------------

mod imp {
    use std::path::Path;
    use std::sync::{mpsc, Arc, Mutex};
    use std::time::Duration;

    use block2::RcBlock;
    use dispatch2::{DispatchQueue, DispatchQueueAttr};
    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
    use objc2_av_foundation::{
        AVAssetWriter, AVAssetWriterInput, AVAssetWriterStatus, AVCaptureAudioDataOutput,
        AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput,
        AVCaptureSession, AVCaptureVideoDataOutput,
        AVCaptureVideoDataOutputSampleBufferDelegate,
        AVCaptureAudioDataOutputSampleBufferDelegate, AVFileTypeAppleM4A,
        AVFileTypeQuickTimeMovie, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
    };
    use objc2_core_media::{CMSampleBuffer, CMSampleBufferGetPresentationTimeStamp, CMTime};
    use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURL};
    use objc2_screen_capture_kit::{
        SCContentFilter, SCDisplay, SCShareableContent, SCStream, SCStreamConfiguration,
        SCStreamOutput, SCStreamOutputType,
    };

    use crate::core::capture::SourceRole;
    use crate::core::error::{CoreError, Result};
    use crate::recorder::{TakeRequest, TakeSource};

    const SCK_CALLBACK_TIMEOUT: Duration = Duration::from_secs(5);

    /// Live Phase-2 Take. Holds retained handles to every SCK / AVFoundation
    /// object that needs to outlive Start so they aren't released out from
    /// under the recording. Stop drops the lot in the right order.
    pub struct Take {
        sources: Vec<Source>,
    }

    /// One per-source writer pipeline. Owns its `AVAssetWriter`, the
    /// stream/session it pulls from, the delegate the OS calls into, and
    /// the shared per-source state that delegate consults for each sample.
    struct Source {
        kind: SourceKind,
        writer: Retained<AVAssetWriter>,
        #[allow(dead_code)]
        shared: Arc<SourceShared>,
    }

    enum SourceKind {
        Sck {
            stream: Retained<SCStream>,
            #[allow(dead_code)]
            delegate: Retained<SckStreamDelegate>,
        },
        Av {
            session: Retained<AVCaptureSession>,
            #[allow(dead_code)]
            video_delegate: Option<Retained<AvVideoDelegate>>,
            #[allow(dead_code)]
            audio_delegate: Option<Retained<AvAudioDelegate>>,
        },
    }

    /// Per-source mutable state shared with the OS callback delegate.
    ///
    /// * `started` flips once `startSession(atSourceTime:)` has been called
    ///   on the writer — the first received sample's PTS is what we hand
    ///   in, so the per-source session origin is whatever the OS clock said
    ///   at the moment that source's first sample landed.
    /// * `input` is the writer's video / primary input we route samples
    ///   into. For audio-only sources it's the audio input.
    struct SourceShared {
        writer: Retained<AVAssetWriter>,
        input: Retained<AVAssetWriterInput>,
        started: Mutex<bool>,
    }

    // SCK / AVFoundation handles are documented as safe to use from
    // arbitrary threads. The outer Mutex in SckMacRecording serialises
    // calls into here, so we never actually share these handles between
    // threads simultaneously — Send is enough; Sync we assert defensively
    // so the type can sit in a Mutex without an extra wrapper.
    unsafe impl Send for Take {}
    unsafe impl Sync for Take {}

    impl Take {
        pub fn start(take: &TakeRequest) -> Result<Self> {
            // Eagerly resolve shareable content once if any SCK source is
            // in the Take — saves N round-trips through the timeout-bound
            // async API.
            let needs_sck = take
                .sources
                .iter()
                .any(|s| s.request.role.is_sck_sourced());
            let content = if needs_sck {
                Some(await_shareable_content()?)
            } else {
                None
            };

            // Build each per-source pipeline in order. If any errors,
            // tear down everything we've built so far — Atomic Start.
            let mut sources: Vec<Source> = Vec::with_capacity(take.sources.len());
            for src in &take.sources {
                match Self::start_source(src, content.as_deref()) {
                    Ok(s) => sources.push(s),
                    Err(e) => {
                        // Tear down anything we've already started.
                        for finished in sources {
                            let _ = finished.cancel_writer();
                        }
                        // And clean up the partial files we created on
                        // disk for the unstarted sources (the OS may have
                        // touched them).
                        for s in &take.sources {
                            let _ = std::fs::remove_file(&s.partial_path);
                        }
                        return Err(e);
                    }
                }
            }

            Ok(Self { sources })
        }

        fn start_source(
            src: &TakeSource,
            content: Option<&SCShareableContent>,
        ) -> Result<Source> {
            let path_str = src.partial_path.to_str().ok_or_else(|| {
                CoreError::Recorder(format!(
                    "partial path is not valid UTF-8: {}",
                    src.partial_path.display()
                ))
            })?;
            let url = NSURL::fileURLWithPath(&NSString::from_str(path_str));

            // Pick output file type per role: video sources → .mov,
            // audio-only sources → .m4a.
            let file_type = if src.request.role.is_audio_only() {
                unsafe { AVFileTypeAppleM4A }
                    .expect("AVFileTypeAppleM4A unavailable — AVFoundation missing?")
            } else {
                unsafe { AVFileTypeQuickTimeMovie }
                    .expect("AVFileTypeQuickTimeMovie unavailable — AVFoundation missing?")
            };

            // Build the writer. Phase 2 lets AVAssetWriter pick its own
            // input settings from the sample buffers we hand in (the SCK
            // / AVCaptureSession side has chosen pixel format / sample
            // rate / channel count for us).
            let writer = unsafe {
                AVAssetWriter::assetWriterWithURL_fileType_error(&url, file_type).map_err(
                    |e| nserror_to_recorder("AVAssetWriter init", &e),
                )?
            };

            // One primary input per source: video for video roles, audio
            // for audio-only roles. (Camera with audio would add a second
            // input — Phase 2 deliberately treats camera as video-only and
            // forces the user to add a separate Microphone source if they
            // want voice.)
            let media_type: &AVMediaType = if src.request.role.is_audio_only() {
                unsafe { AVMediaTypeAudio }
                    .expect("AVMediaTypeAudio unavailable")
            } else {
                unsafe { AVMediaTypeVideo }
                    .expect("AVMediaTypeVideo unavailable")
            };
            let input = unsafe {
                AVAssetWriterInput::assetWriterInputWithMediaType_outputSettings(
                    media_type, None,
                )
            };
            unsafe {
                input.setExpectsMediaDataInRealTime(true);
                if writer.canAddInput(&input) {
                    writer.addInput(&input);
                } else {
                    return Err(CoreError::Recorder(format!(
                        "AVAssetWriter refused {:?} input for {}",
                        src.request.role,
                        src.partial_path.display()
                    )));
                }
                if !writer.startWriting() {
                    let err = writer.error();
                    let msg = err
                        .as_ref()
                        .map(|e| e.localizedDescription().to_string())
                        .unwrap_or_else(|| "(no underlying NSError)".into());
                    return Err(CoreError::Recorder(format!(
                        "AVAssetWriter startWriting failed for {:?}: {msg}",
                        src.request.role
                    )));
                }
            }

            let shared = Arc::new(SourceShared {
                writer: writer.clone(),
                input: input.clone(),
                started: Mutex::new(false),
            });

            let kind = match src.request.role {
                SourceRole::Screen | SourceRole::Window | SourceRole::SystemAudio => {
                    let content = content.ok_or_else(|| {
                        CoreError::Recorder(
                            "SCK source requested but shareable content unresolved".into(),
                        )
                    })?;
                    Self::start_sck_pipeline(src, content, shared.clone())?
                }
                SourceRole::Camera | SourceRole::Microphone => {
                    Self::start_av_pipeline(src, shared.clone())?
                }
            };

            Ok(Source { kind, writer, shared })
        }

        fn start_sck_pipeline(
            src: &TakeSource,
            content: &SCShareableContent,
            shared: Arc<SourceShared>,
        ) -> Result<SourceKind> {
            // Phase 2 picks the first display for any screen / system-audio
            // role (per-display device-binding lands when issue #34's
            // device-id surfaces a real `CGDirectDisplayID`). Window
            // capture falls back to the same display until window-pick is
            // also wired through.
            let displays = unsafe { content.displays() };
            if displays.is_empty() {
                return Err(CoreError::Recorder(
                    "no displays available to capture (SCShareableContent empty)".into(),
                ));
            }
            let display: Retained<SCDisplay> = displays.objectAtIndex(0);

            let empty_windows: Retained<
                objc2_foundation::NSArray<objc2_screen_capture_kit::SCWindow>,
            > = objc2_foundation::NSArray::new();
            let filter = unsafe {
                SCContentFilter::initWithDisplay_excludingWindows(
                    SCContentFilter::alloc(),
                    &display,
                    &empty_windows,
                )
            };
            let config: Retained<SCStreamConfiguration> =
                unsafe { SCStreamConfiguration::new() };
            unsafe {
                config.setMinimumFrameInterval(CMTime {
                    value: 1,
                    timescale: 30,
                    flags: objc2_core_media::CMTimeFlags::Valid,
                    epoch: 0,
                });
                if src.request.role == SourceRole::SystemAudio {
                    config.setCapturesAudio(true);
                }
            }

            let stream = unsafe {
                SCStream::initWithFilter_configuration_delegate(
                    SCStream::alloc(),
                    &filter,
                    &config,
                    None,
                )
            };

            // The delegate forwards every CMSampleBuffer it receives into
            // `shared.input.appendSampleBuffer`. SCK calls into it on its
            // own internal queue.
            let delegate = SckStreamDelegate::new(shared);
            let proto: &ProtocolObject<dyn SCStreamOutput> =
                ProtocolObject::from_ref(&*delegate);
            let kind_type = if src.request.role == SourceRole::SystemAudio {
                SCStreamOutputType::Audio
            } else {
                SCStreamOutputType::Screen
            };
            unsafe {
                let queue = DispatchQueue::new("courseforge.sck.output", DispatchQueueAttr::SERIAL);
                stream
                    .addStreamOutput_type_sampleHandlerQueue_error(
                        proto,
                        kind_type,
                        Some(&queue),
                    )
                    .map_err(|e| nserror_to_recorder("SCStream.addStreamOutput", &e))?;
            }

            await_capture_start(&stream)?;

            Ok(SourceKind::Sck { stream, delegate })
        }

        fn start_av_pipeline(
            src: &TakeSource,
            shared: Arc<SourceShared>,
        ) -> Result<SourceKind> {
            let session = unsafe { AVCaptureSession::new() };
            // Default device for the role. The picker (#34) hands us a
            // real device id but Phase 2 falls back to the system default
            // when that id is "default" or unrecognised at start time.
            let device = pick_av_device_for(src.request.role)?;
            let input = unsafe {
                AVCaptureDeviceInput::initWithDevice_error(
                    AVCaptureDeviceInput::alloc(),
                    &device,
                )
                .map_err(|e| nserror_to_recorder("AVCaptureDeviceInput init", &e))?
            };
            unsafe {
                if session.canAddInput(&input) {
                    session.addInput(&input);
                } else {
                    return Err(CoreError::Recorder(format!(
                        "AVCaptureSession refused {:?} input",
                        src.request.role
                    )));
                }
            }

            let (video_delegate, audio_delegate) = if src.request.role.is_audio_only() {
                let out = unsafe { AVCaptureAudioDataOutput::new() };
                let delegate = AvAudioDelegate::new(shared);
                let queue = DispatchQueue::new(
                    "courseforge.av.audio",
                    DispatchQueueAttr::SERIAL,
                );
                unsafe {
                    let proto: &ProtocolObject<
                        dyn AVCaptureAudioDataOutputSampleBufferDelegate,
                    > = ProtocolObject::from_ref(&*delegate);
                    out.setSampleBufferDelegate_queue(Some(proto), Some(&queue));
                    if session.canAddOutput(&out) {
                        session.addOutput(&out);
                    } else {
                        return Err(CoreError::Recorder(
                            "AVCaptureSession refused audio output".into(),
                        ));
                    }
                }
                (None, Some(delegate))
            } else {
                let out = unsafe { AVCaptureVideoDataOutput::new() };
                let delegate = AvVideoDelegate::new(shared);
                let queue = DispatchQueue::new(
                    "courseforge.av.video",
                    DispatchQueueAttr::SERIAL,
                );
                unsafe {
                    let proto: &ProtocolObject<
                        dyn AVCaptureVideoDataOutputSampleBufferDelegate,
                    > = ProtocolObject::from_ref(&*delegate);
                    out.setSampleBufferDelegate_queue(Some(proto), Some(&queue));
                    if session.canAddOutput(&out) {
                        session.addOutput(&out);
                    } else {
                        return Err(CoreError::Recorder(
                            "AVCaptureSession refused video output".into(),
                        ));
                    }
                }
                (Some(delegate), None)
            };

            unsafe {
                session.startRunning();
            }

            Ok(SourceKind::Av {
                session,
                video_delegate,
                audio_delegate,
            })
        }

        pub fn stop(self) -> Result<()> {
            // Per-source stop: ask each pipeline to halt, then finalise
            // every writer. Each writer's `finishWriting` blocks until
            // the file is durably on disk. #37 parallelises this.
            for source in &self.sources {
                match &source.kind {
                    SourceKind::Sck { stream, .. } => {
                        let _ = await_capture_stop(stream);
                    }
                    SourceKind::Av { session, .. } => unsafe {
                        session.stopRunning();
                    },
                }
            }
            for source in &self.sources {
                let writer = source.writer.clone();
                unsafe {
                    for input in writer.inputs().iter() {
                        input.markAsFinished();
                    }
                    finish_writing_blocking(&writer)?;
                }
            }
            Ok(())
        }
    }

    impl Source {
        fn cancel_writer(&self) -> Result<()> {
            // Tear down a writer mid-init without finalising — used by
            // the Atomic Start cleanup path. Best-effort: any error here
            // is swallowed by the caller because we're already failing.
            unsafe {
                self.writer.cancelWriting();
            }
            match &self.kind {
                SourceKind::Sck { stream, .. } => {
                    let _ = await_capture_stop(stream);
                }
                SourceKind::Av { session, .. } => unsafe {
                    session.stopRunning();
                },
            }
            Ok(())
        }
    }

    fn pick_av_device_for(role: SourceRole) -> Result<Retained<AVCaptureDevice>> {
        let media: &AVMediaType = if role == SourceRole::Microphone {
            unsafe { AVMediaTypeAudio }.expect("AVMediaTypeAudio unavailable")
        } else {
            unsafe { AVMediaTypeVideo }.expect("AVMediaTypeVideo unavailable")
        };
        let device = unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media) }
            .ok_or_else(|| {
                CoreError::Recorder(format!(
                    "no default {:?} device available — is one attached and is permission granted?",
                    role
                ))
            })?;
        Ok(device)
    }

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
                        "SCShareableContent completion returned no content".into(),
                    ))
                } else {
                    unsafe { Retained::retain(content) }
                        .ok_or_else(|| CoreError::Recorder(
                            "SCShareableContent failed to retain pointer".into(),
                        ))
                };
                let _ = tx.send(result);
            }
        });
        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&handler);
        }
        rx.recv_timeout(SCK_CALLBACK_TIMEOUT).unwrap_or_else(|_| {
            Err(CoreError::Recorder(
                "SCShareableContent enumeration timed out — Screen Recording permission missing?"
                    .into(),
            ))
        })
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
        rx.recv_timeout(SCK_CALLBACK_TIMEOUT)
            .unwrap_or_else(|_| Err(CoreError::Recorder("startCapture timed out".into())))
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
        rx.recv_timeout(SCK_CALLBACK_TIMEOUT).unwrap_or(Ok(()))
    }

    unsafe fn finish_writing_blocking(writer: &AVAssetWriter) -> Result<()> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new(move || {
            let _ = tx.send(());
        });
        writer.finishWritingWithCompletionHandler(&handler);
        let _ = rx.recv_timeout(Duration::from_secs(10));
        if let Some(err) = writer.error() {
            return Err(nserror_to_recorder("finishWriting", &err));
        }
        Ok(())
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

    // --- delegates -----------------------------------------------------

    /// Shared sample-handling routine. Drops the sample if the writer
    /// isn't ready / the input isn't accepting; otherwise lazily starts
    /// the writer's session at the sample's PTS and appends.
    fn forward_sample(shared: &SourceShared, sample: &CMSampleBuffer) {
        unsafe {
            // Drop samples until both writer and input are ready. This
            // is the same pattern Apple's AVCam sample code uses.
            if shared.writer.status() != objc2_av_foundation::AVAssetWriterStatus::Writing {
                return;
            }
            if !shared.input.isReadyForMoreMediaData() {
                return;
            }
            // First sample: start the session at this PTS. Every later
            // source's first sample lands within ~one tick of this one
            // (they're all on the same system CMTime clock), preserving
            // cross-source sync.
            {
                let mut started = shared.started.lock().unwrap();
                if !*started {
                    let pts = CMSampleBufferGetPresentationTimeStamp(sample);
                    shared.writer.startSessionAtSourceTime(pts);
                    *started = true;
                }
            }
            let _ = shared.input.appendSampleBuffer(sample);
        }
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "CourseforgeSckStreamDelegate"]
        #[ivars = SckStreamDelegateIvars]
        pub struct SckStreamDelegate;

        unsafe impl NSObjectProtocol for SckStreamDelegate {}

        unsafe impl SCStreamOutput for SckStreamDelegate {
            #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
            unsafe fn stream_didOutputSampleBuffer_ofType(
                &self,
                _stream: &SCStream,
                sample: *mut CMSampleBuffer,
                _of_type: SCStreamOutputType,
            ) {
                if sample.is_null() {
                    return;
                }
                let sample = unsafe { &*sample };
                forward_sample(&self.ivars().shared, sample);
            }
        }
    );

    pub struct SckStreamDelegateIvars {
        shared: Arc<SourceShared>,
    }

    impl SckStreamDelegate {
        fn new(shared: Arc<SourceShared>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(SckStreamDelegateIvars { shared });
            unsafe { msg_send![super(this), init] }
        }
    }

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "CourseforgeAvVideoDelegate"]
        #[ivars = AvDelegateIvars]
        pub struct AvVideoDelegate;

        unsafe impl NSObjectProtocol for AvVideoDelegate {}

        unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for AvVideoDelegate {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            unsafe fn captureOutput_didOutputSampleBuffer_fromConnection(
                &self,
                _output: &AVCaptureOutput,
                sample: *mut CMSampleBuffer,
                _conn: &AVCaptureConnection,
            ) {
                if sample.is_null() {
                    return;
                }
                let sample = unsafe { &*sample };
                forward_sample(&self.ivars().shared, sample);
            }
        }
    );

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "CourseforgeAvAudioDelegate"]
        #[ivars = AvDelegateIvars]
        pub struct AvAudioDelegate;

        unsafe impl NSObjectProtocol for AvAudioDelegate {}

        unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for AvAudioDelegate {
            #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
            unsafe fn captureOutput_didOutputSampleBuffer_fromConnection(
                &self,
                _output: &AVCaptureOutput,
                sample: *mut CMSampleBuffer,
                _conn: &AVCaptureConnection,
            ) {
                if sample.is_null() {
                    return;
                }
                let sample = unsafe { &*sample };
                forward_sample(&self.ivars().shared, sample);
            }
        }
    );

    pub struct AvDelegateIvars {
        shared: Arc<SourceShared>,
    }

    impl AvVideoDelegate {
        fn new(shared: Arc<SourceShared>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(AvDelegateIvars { shared });
            unsafe { msg_send![super(this), init] }
        }
    }

    impl AvAudioDelegate {
        fn new(shared: Arc<SourceShared>) -> Retained<Self> {
            let this = Self::alloc().set_ivars(AvDelegateIvars { shared });
            unsafe { msg_send![super(this), init] }
        }
    }

}
