//! Live device enumeration for the Scene Editor's per-source dropdown.
//!
//! Per ADR-0002, each `SceneSource` binds a [`SourceRole`] to a specific
//! `Device { id, label }`. Slice 1 (issue #33) used the placeholder
//! `Device { id: "default", … }` for every row; this slice asks the OS for
//! the real device list per role at call time so the user can pick.
//!
//! Backend dispatch:
//!
//! * `Screen` → `SCShareableContent.displays` — one entry per attached
//!   display, id = `CGDirectDisplayID` stringified, label = a
//!   "Display N (WxH)" rendering.
//! * `Window` → `SCShareableContent.windows` filtered to ones with a
//!   non-empty owner application — one entry per shareable window, id =
//!   `kCGWindowNumber` stringified, label = "App — Window Title".
//! * `Camera` / `Microphone` → `AVCaptureDevice.devicesWithMediaType`
//!   for `video` / `audio` respectively, id = device's uniqueID, label =
//!   `localizedName`.
//! * `SystemAudio` → mirrors `Screen` (SCK ties system audio to a display).
//!
//! On non-macOS, every call returns an empty list — same shape as a Mac
//! with no displays / cameras attached — so callers never panic.
//!
//! Tests below cover the *non-macOS* and the *empty-list-is-not-an-error*
//! contracts. The live macOS path has an `#[ignore]`'d integration test
//! that CI can't guarantee a built-in camera for.

use crate::core::capture::{Device, SourceRole};
use crate::core::error::Result;

/// Return the device list for `role`, queried live from the OS.
///
/// Always returns Ok; an empty list is not an error (it just means the
/// machine has none of that role's hardware right now — the Scene Editor
/// should render "no devices found" rather than crash).
pub fn list_capture_devices(role: SourceRole) -> Result<Vec<Device>> {
    #[cfg(target_os = "macos")]
    {
        mac::list(role)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = role;
        Ok(Vec::new())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_av_foundation::{
        AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceType,
        AVCaptureDeviceTypeMicrophone, AVCaptureDeviceTypeBuiltInWideAngleCamera,
        AVCaptureDeviceTypeContinuityCamera, AVCaptureDeviceTypeDeskViewCamera,
        AVCaptureDeviceTypeExternal, AVMediaType, AVMediaTypeAudio, AVMediaTypeVideo,
    };
    use objc2_foundation::{NSArray, NSError};
    use objc2_screen_capture_kit::{SCShareableContent, SCWindow};

    use crate::core::capture::{Device, SourceRole};
    use crate::core::error::{CoreError, Result};

    const ENUMERATION_TIMEOUT: Duration = Duration::from_secs(5);

    pub fn list(role: SourceRole) -> Result<Vec<Device>> {
        match role {
            SourceRole::Screen | SourceRole::SystemAudio => list_displays(),
            SourceRole::Window => list_windows(),
            SourceRole::Camera => {
                let media: &AVMediaType = unsafe { AVMediaTypeVideo }
                    .expect("AVMediaTypeVideo unavailable — AVFoundation missing?");
                let types: Vec<&'static AVCaptureDeviceType> = unsafe {
                    vec![
                        AVCaptureDeviceTypeBuiltInWideAngleCamera,
                        AVCaptureDeviceTypeContinuityCamera,
                        AVCaptureDeviceTypeDeskViewCamera,
                        AVCaptureDeviceTypeExternal,
                    ]
                };
                list_av_devices(media, &types)
            }
            SourceRole::Microphone => {
                let media: &AVMediaType = unsafe { AVMediaTypeAudio }
                    .expect("AVMediaTypeAudio unavailable — AVFoundation missing?");
                let types: Vec<&'static AVCaptureDeviceType> =
                    unsafe { vec![AVCaptureDeviceTypeMicrophone] };
                list_av_devices(media, &types)
            }
        }
    }

    fn list_displays() -> Result<Vec<Device>> {
        let content = match await_shareable_content() {
            Ok(c) => c,
            // Permission denied / not granted yet: surface as an empty
            // list, not an error, so the Scene Editor renders "no devices
            // found" instead of crashing.
            Err(_) => return Ok(Vec::new()),
        };
        let displays = unsafe { content.displays() };
        let mut out = Vec::with_capacity(displays.len());
        for d in displays.iter() {
            let id: u32 = unsafe { d.displayID() };
            let width: isize = unsafe { d.width() } as isize;
            let height: isize = unsafe { d.height() } as isize;
            out.push(Device {
                id: id.to_string(),
                label: format!("Display {id} ({width}×{height})"),
            });
        }
        Ok(out)
    }

    fn list_windows() -> Result<Vec<Device>> {
        let content = match await_shareable_content() {
            Ok(c) => c,
            Err(_) => return Ok(Vec::new()),
        };
        let windows: Retained<NSArray<SCWindow>> = unsafe { content.windows() };
        let mut out = Vec::with_capacity(windows.len());
        for w in windows.iter() {
            // Skip windows without an owner app — they're system /
            // shadow-window noise the user can't usefully record.
            let owner = unsafe { w.owningApplication() };
            let owner_name = owner
                .as_ref()
                .map(|a| unsafe { a.applicationName() }.to_string())
                .unwrap_or_default();
            if owner_name.is_empty() {
                continue;
            }
            let id: u32 = unsafe { w.windowID() };
            let title = unsafe { w.title() }
                .as_ref()
                .map(|t| t.to_string())
                .unwrap_or_default();
            let label = if title.is_empty() {
                owner_name.clone()
            } else {
                format!("{owner_name} — {title}")
            };
            out.push(Device {
                id: id.to_string(),
                label,
            });
        }
        Ok(out)
    }

    fn list_av_devices(
        media_type: &AVMediaType,
        device_types: &[&'static AVCaptureDeviceType],
    ) -> Result<Vec<Device>> {
        // AVCaptureDevice.devicesWithMediaType was deprecated in favour of
        // AVCaptureDeviceDiscoverySession on modern macOS — the discovery
        // session is what Apple recommends for Catalina+ and gives us a
        // stable enumeration that includes Continuity Camera and external
        // USB devices via the right device-type list.
        let types: Retained<NSArray<AVCaptureDeviceType>> =
            NSArray::from_slice(device_types);
        let session = unsafe {
            AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
                &types,
                Some(media_type),
                objc2_av_foundation::AVCaptureDevicePosition::Unspecified,
            )
        };
        let devices: Retained<NSArray<AVCaptureDevice>> = unsafe { session.devices() };
        let mut out = Vec::with_capacity(devices.len());
        for d in devices.iter() {
            let id = unsafe { d.uniqueID() }.to_string();
            let label = unsafe { d.localizedName() }.to_string();
            out.push(Device { id, label });
        }
        Ok(out)
    }

    fn await_shareable_content() -> Result<Retained<SCShareableContent>> {
        let (tx, rx) = mpsc::channel();
        let handler = RcBlock::new({
            let tx = tx.clone();
            move |content: *mut SCShareableContent, error: *mut NSError| {
                let result: Result<Retained<SCShareableContent>> = if !error.is_null() {
                    let err = unsafe { &*error };
                    Err(CoreError::Recorder(format!(
                        "SCShareableContent failed: {}",
                        err.localizedDescription().to_string()
                    )))
                } else if content.is_null() {
                    Err(CoreError::Recorder(
                        "SCShareableContent returned no content".into(),
                    ))
                } else {
                    unsafe { Retained::retain(content) }.ok_or_else(|| {
                        CoreError::Recorder(
                            "SCShareableContent: failed to retain pointer".into(),
                        )
                    })
                };
                let _ = tx.send(result);
            }
        });
        unsafe {
            SCShareableContent::getShareableContentWithCompletionHandler(&handler);
        }
        match rx.recv_timeout(ENUMERATION_TIMEOUT) {
            Ok(r) => r,
            Err(_) => Err(CoreError::Recorder(
                "SCShareableContent enumeration timed out".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_capture_devices_never_panics_for_any_role() {
        // Empty-list-is-not-an-error contract: even on a host with no
        // hardware of a given role, the call returns Ok with an empty Vec
        // rather than blowing up.
        for role in [
            SourceRole::Screen,
            SourceRole::Window,
            SourceRole::Camera,
            SourceRole::Microphone,
            SourceRole::SystemAudio,
        ] {
            let _ = list_capture_devices(role).unwrap();
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn non_macos_returns_empty_lists_for_every_role() {
        for role in [
            SourceRole::Screen,
            SourceRole::Window,
            SourceRole::Camera,
            SourceRole::Microphone,
            SourceRole::SystemAudio,
        ] {
            let devices = list_capture_devices(role).unwrap();
            assert!(devices.is_empty(), "non-mac should return empty for {:?}", role);
        }
    }

    // Live SCK / AVCaptureDevice enumeration on macOS hosts. CI cannot
    // guarantee a built-in camera, so this is `#[ignore]`'d; run manually
    // with `cargo test list_capture_devices_camera -- --ignored` on a Mac
    // that has one.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore = "needs a Mac with a built-in camera + Screen Recording permission"]
    fn list_capture_devices_camera_returns_at_least_one_entry_on_a_mac_with_a_built_in_camera() {
        let cameras = list_capture_devices(SourceRole::Camera).unwrap();
        assert!(!cameras.is_empty(), "expected at least one camera on this Mac");
        for c in &cameras {
            assert!(!c.id.is_empty());
            assert!(!c.label.is_empty());
        }
    }
}
