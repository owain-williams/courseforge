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
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::core::capture::SourceRole;
use crate::core::error::{CoreError, Result};
use super::{ActiveRecording, RecorderBackend, SourceOutcome, TakeRequest, TakeSource};

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
        let paused = Arc::new(AtomicBool::new(false));
        let take_state = catch_panic("SckMacBackend::start", || {
            imp::Take::start(&take, paused.clone())
        })?;
        Ok(Box::new(SckMacRecording {
            partial_paths: take
                .sources
                .iter()
                .map(|s| s.partial_path.clone())
                .collect(),
            take: Mutex::new(Some(take_state)),
            paused,
        }))
    }
}

pub struct SckMacRecording {
    #[allow(dead_code)]
    partial_paths: Vec<PathBuf>,
    take: Mutex<Option<imp::Take>>,
    /// Shared atomic the per-source sample-buffer delegates consult before
    /// calling `appendSampleBuffer`. When `true`, samples are dropped — so
    /// the visible-on-playback gap at the pause seam is identical across
    /// every source in the Take (issue #37).
    paused: Arc<AtomicBool>,
}

impl ActiveRecording for SckMacRecording {
    fn pause(&self) -> Result<()> {
        self.paused.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn resume(&self) -> Result<()> {
        self.paused.store(false, Ordering::SeqCst);
        Ok(())
    }

    fn stop(&self) -> Result<Vec<SourceOutcome>> {
        let take = self.take.lock().unwrap().take();
        if let Some(take) = take {
            return catch_panic("SckMacBackend::stop", move || take.stop());
        }
        Ok(Vec::new())
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
                is_transcript_source: false,
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
    use std::sync::atomic::{AtomicBool, Ordering};
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

    use crate::core::capture::{EndedReason, SourceRole};
    use crate::core::error::{CoreError, Result};
    use crate::recorder::{SourceOutcome, TakeRequest, TakeSource};

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
        segment_id: String,
        kind: SourceKind,
        writer: Retained<AVAssetWriter>,
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
    /// * `paused` is the Take-wide atomic the manager flips on Pause /
    ///   Resume. Every source observes it on the same memory location, so
    ///   the visible gap at the pause seam lands at the same PTS across
    ///   every track in the Take (issue #37).
    /// * `failed` records a mid-Take per-source failure so the manager
    ///   emits `endedReason: sourceFailed` instead of `normal` for that
    ///   source's sidecar.
    struct SourceShared {
        writer: Retained<AVAssetWriter>,
        input: Retained<AVAssetWriterInput>,
        started: Mutex<bool>,
        paused: Arc<AtomicBool>,
        failed: Mutex<Option<String>>,
    }

    // Safety: AVAssetWriter / AVAssetWriterInput are documented as safe
    // to call from any thread once startWriting has been issued (Apple's
    // AVAssetWriter docs). All mutable access in this module is gated by
    // the `started` and `failed` mutexes.
    unsafe impl Send for SourceShared {}
    unsafe impl Sync for SourceShared {}

    /// Thread-safe wrapper around `Retained<AVAssetWriter>` so the per-
    /// source finishWriting thread can own the handle. Apple documents
    /// finishWriting / markAsFinished as safe to call from any thread.
    pub struct SendableWriter(pub Retained<AVAssetWriter>);
    unsafe impl Send for SendableWriter {}
    unsafe impl Sync for SendableWriter {}

    // SCK / AVFoundation handles are documented as safe to use from
    // arbitrary threads. The outer Mutex in SckMacRecording serialises
    // calls into here, so we never actually share these handles between
    // threads simultaneously — Send is enough; Sync we assert defensively
    // so the type can sit in a Mutex without an extra wrapper.
    unsafe impl Send for Take {}
    unsafe impl Sync for Take {}

    impl Take {
        pub fn start(take: &TakeRequest, paused: Arc<AtomicBool>) -> Result<Self> {
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
                match Self::start_source(src, content.as_deref(), paused.clone()) {
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
            paused: Arc<AtomicBool>,
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
                paused: paused.clone(),
                failed: Mutex::new(None),
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

            Ok(Source {
                segment_id: src.segment_id.clone(),
                kind,
                writer,
                shared,
            })
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

        pub fn stop(self) -> Result<Vec<SourceOutcome>> {
            // Ask each capture pipeline to halt first — once the stream /
            // session stops, no more sample buffers are produced.
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

            // Finalise each writer in parallel — `finishWriting` is the
            // slow step (the OS flushes the mdat/moov atoms to disk).
            // Issuing them concurrently means Stop returns once the
            // slowest single writer is durable rather than the sum of all
            // of them (#37 AC).
            //
            // AVAssetWriter handles aren't `Send` per the autoderived
            // bounds, so we ferry them across the thread boundary in a
            // `SendableWriter` newtype (defined at module scope above).
            let n = self.sources.len();
            let mut handles = Vec::with_capacity(n);
            for (idx, source) in self.sources.iter().enumerate() {
                let writer = SendableWriter(source.writer.clone());
                let shared = source.shared.clone();
                handles.push(std::thread::spawn(move || -> (usize, Result<()>) {
                    // Bind the whole SendableWriter into the closure so
                    // Rust's precise-capture analysis sees the wrapper
                    // (which is `unsafe impl Send`), not just `writer.0`
                    // (which is `Retained<AVAssetWriter>` and isn't).
                    let writer = writer;
                    let r = unsafe {
                        for input in writer.0.inputs().iter() {
                            input.markAsFinished();
                        }
                        finish_writing_blocking(&writer.0)
                    };
                    if let Err(ref e) = r {
                        let mut guard = shared.failed.lock().unwrap();
                        if guard.is_none() {
                            *guard = Some(format!("finishWriting: {e}"));
                        }
                    }
                    (idx, r)
                }));
            }

            // Collect outcomes in the original source order. Per-writer
            // failures don't abort the others — they degrade to a
            // per-source SourceFailed outcome so the rest of the Take
            // survives (issue #37 mid-Take per-source failure rule).
            let mut outcomes: Vec<Option<SourceOutcome>> = (0..n).map(|_| None).collect();
            for h in handles {
                let (idx, _r) = h.join().unwrap_or((usize::MAX, Ok(())));
                if idx == usize::MAX {
                    continue;
                }
                let source = &self.sources[idx];
                let failed = source.shared.failed.lock().unwrap();
                let outcome = if failed.is_some() {
                    SourceOutcome {
                        segment_id: source.segment_id.clone(),
                        ended_reason: EndedReason::SourceFailed,
                        ended_at: Some(iso8601_now()),
                    }
                } else {
                    SourceOutcome {
                        segment_id: source.segment_id.clone(),
                        ended_reason: EndedReason::Normal,
                        ended_at: None,
                    }
                };
                outcomes[idx] = Some(outcome);
            }
            Ok(outcomes.into_iter().flatten().collect())
        }
    }

    fn iso8601_now() -> String {
        // Same ISO-8601 shape the manager uses for `recorded_at`.
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let secs_per_day = 86_400u64;
        let days = secs / secs_per_day;
        let rem = secs % secs_per_day;
        let h = rem / 3600;
        let m = (rem % 3600) / 60;
        let s = rem % 60;
        let z = days as i64 + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = (z - era * 146_097) as u64;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe as i64 + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let mo = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if mo <= 2 { y + 1 } else { y };
        format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, mo, d, h, m, s)
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

    /// Shared sample-handling routine. Drops the sample if:
    ///
    /// * The Take is paused (atomic gate flipped by the manager).
    /// * The source has already been marked failed mid-Take.
    /// * The writer isn't in `Writing` status or the input isn't ready.
    ///
    /// Otherwise lazily starts the writer's session at the sample's PTS
    /// and appends. If `appendSampleBuffer` returns false, treat that as
    /// a per-source failure — record it on `shared.failed` so the Take's
    /// final outcome carries `SourceFailed` while the rest of the Take
    /// continues uninterrupted (issue #37 mid-Take rule).
    fn forward_sample(shared: &SourceShared, sample: &CMSampleBuffer) {
        // Pause gate — drop samples while paused. Both pause and resume
        // are SeqCst stores from the manager so every delegate sees the
        // flip on the same PTS interval.
        if shared.paused.load(Ordering::SeqCst) {
            return;
        }
        // If we've already failed, stop appending so the file isn't
        // contaminated by post-failure samples.
        if shared.failed.lock().unwrap().is_some() {
            return;
        }
        unsafe {
            if shared.writer.status() != objc2_av_foundation::AVAssetWriterStatus::Writing {
                // Writer entered an error / finished state mid-Take —
                // record it as a per-source failure so we surface
                // SourceFailed in the outcome.
                let mut failed = shared.failed.lock().unwrap();
                if failed.is_none() {
                    *failed = Some(format!(
                        "writer entered non-Writing status {:?}",
                        shared.writer.status()
                    ));
                }
                return;
            }
            if !shared.input.isReadyForMoreMediaData() {
                return;
            }
            {
                let mut started = shared.started.lock().unwrap();
                if !*started {
                    let pts = CMSampleBufferGetPresentationTimeStamp(sample);
                    shared.writer.startSessionAtSourceTime(pts);
                    *started = true;
                }
            }
            let ok = shared.input.appendSampleBuffer(sample);
            if !ok {
                let mut failed = shared.failed.lock().unwrap();
                if failed.is_none() {
                    *failed = Some("appendSampleBuffer returned false".into());
                }
            }
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
