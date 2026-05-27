//! macOS capture permissions, abstracted so the rest of the app can ask
//! "may we record?" without caring whether we're on macOS or a test rig.
//!
//! TCC (Apple's privacy framework) does not let an app *grant* permission
//! programmatically — only the user, in System Settings, can do that. The
//! best we can do is detect status and deep-link the user to the right pane.

use serde::{Deserialize, Serialize};

use crate::core::capture::{CaptureRequest, SourceRole};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Permission {
    ScreenRecording,
    Camera,
    Microphone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionStatus {
    Granted,
    Denied,
    /// Permission has never been requested — the system will prompt on
    /// first use and we should treat that as not-yet-usable but not fatal.
    NotDetermined,
    /// Restricted by MDM / parental controls etc. — surface to the user as
    /// "ask your admin" rather than "open Settings".
    Restricted,
}

impl PermissionStatus {
    pub fn is_granted(self) -> bool {
        matches!(self, PermissionStatus::Granted)
    }
}

/// Source-of-truth for permission status. Trait-based so domain code stays
/// testable and the macOS-specific TCC calls only run on macOS.
pub trait PermissionChecker: Send + Sync {
    fn status(&self, p: Permission) -> PermissionStatus;
}

/// Snapshot the three permissions Record cares about, for the preflight UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionsSnapshot {
    #[serde(rename = "screenRecording")]
    pub screen_recording: PermissionStatus,
    pub camera: PermissionStatus,
    pub microphone: PermissionStatus,
}

impl PermissionsSnapshot {
    pub fn capture(checker: &dyn PermissionChecker) -> Self {
        Self {
            screen_recording: checker.status(Permission::ScreenRecording),
            camera: checker.status(Permission::Camera),
            microphone: checker.status(Permission::Microphone),
        }
    }

    /// True iff every permission required by the supplied capture requests
    /// is granted. Each [`SourceRole`] maps to the OS-level permission it
    /// needs; a request whose permission is missing means we must hard-fail
    /// the Take Start so no source spins up only to fail silently halfway.
    pub fn satisfies_requests(&self, requests: &[CaptureRequest]) -> bool {
        requests.iter().all(|r| self.status_for(r.role).is_granted())
    }

    /// Which permission this snapshot reports for a given role. `Window`
    /// and `SystemAudio` ride on Screen Recording (they're SCK content);
    /// `Camera` and `Microphone` are their own TCC slots.
    pub fn status_for(&self, role: SourceRole) -> PermissionStatus {
        match role {
            SourceRole::Screen | SourceRole::Window | SourceRole::SystemAudio => {
                self.screen_recording
            }
            SourceRole::Camera => self.camera,
            SourceRole::Microphone => self.microphone,
        }
    }
}

/// Which pane "Open System Settings" should target. The values are the
/// well-known TCC anchor strings macOS understands when opened via
/// `x-apple.systempreferences:com.apple.preference.security?Privacy_…`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SettingsPane {
    ScreenRecording,
    Camera,
    Microphone,
}

impl SettingsPane {
    pub fn for_permission(p: Permission) -> Self {
        match p {
            Permission::ScreenRecording => SettingsPane::ScreenRecording,
            Permission::Camera => SettingsPane::Camera,
            Permission::Microphone => SettingsPane::Microphone,
        }
    }

    /// The `x-apple.systempreferences:` URL that jumps the user straight to
    /// the relevant Privacy & Security pane.
    pub fn url(self) -> &'static str {
        match self {
            SettingsPane::ScreenRecording =>
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture",
            SettingsPane::Camera =>
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Camera",
            SettingsPane::Microphone =>
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        }
    }
}

/// macOS-backed checker. ScreenRecording is queried via CoreGraphics'
/// `CGPreflightScreenCaptureAccess` (returns whether access is already
/// granted, *without* prompting). Camera and Microphone status come from
/// `AVCaptureDevice.authorizationStatus(for:)` via `objc2`, so multi-source
/// Start can fail precisely on the unauthorized device instead of partially
/// starting and silently dropping it.
#[cfg(target_os = "macos")]
pub struct MacPermissionChecker;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
}

#[cfg(target_os = "macos")]
fn permission_status_from_av(
    status: objc2_av_foundation::AVAuthorizationStatus,
) -> PermissionStatus {
    use objc2_av_foundation::AVAuthorizationStatus;
    match status {
        AVAuthorizationStatus::Authorized => PermissionStatus::Granted,
        AVAuthorizationStatus::Denied => PermissionStatus::Denied,
        AVAuthorizationStatus::NotDetermined => PermissionStatus::NotDetermined,
        AVAuthorizationStatus::Restricted => PermissionStatus::Restricted,
        // AVAuthorizationStatus is `#[repr(transparent)]` over an NSInteger,
        // so a future macOS could in principle hand us a value we don't
        // know. Treat unknown as Denied — the safe default for capture.
        _ => PermissionStatus::Denied,
    }
}

#[cfg(target_os = "macos")]
fn av_capture_status(media_type: &objc2_foundation::NSString) -> PermissionStatus {
    // Safety: `authorizationStatusForMediaType` is a thread-safe class
    // method that reads TCC state without prompting. The media-type pointer
    // is a framework constant with `'static` lifetime.
    let raw = unsafe {
        objc2_av_foundation::AVCaptureDevice::authorizationStatusForMediaType(media_type)
    };
    permission_status_from_av(raw)
}

#[cfg(target_os = "macos")]
impl PermissionChecker for MacPermissionChecker {
    fn status(&self, p: Permission) -> PermissionStatus {
        match p {
            Permission::ScreenRecording => {
                // Safety: thin C function, no callback / lifetime concerns.
                if unsafe { CGPreflightScreenCaptureAccess() } {
                    PermissionStatus::Granted
                } else {
                    PermissionStatus::Denied
                }
            }
            Permission::Camera => {
                // Safety: `AVMediaTypeVideo` is a framework-exported NSString
                // constant; accessing it after AVFoundation is linked is sound.
                let media = unsafe { objc2_av_foundation::AVMediaTypeVideo }
                    .expect("AVMediaTypeVideo unavailable on this macOS");
                av_capture_status(media)
            }
            Permission::Microphone => {
                // Safety: same as Camera; `AVMediaTypeAudio` is a static constant.
                let media = unsafe { objc2_av_foundation::AVMediaTypeAudio }
                    .expect("AVMediaTypeAudio unavailable on this macOS");
                av_capture_status(media)
            }
        }
    }
}

/// Pick the right checker for the current platform. On non-macOS the fixed
/// "all granted" checker keeps the rest of the app compiling.
pub fn default_checker() -> Box<dyn PermissionChecker> {
    #[cfg(target_os = "macos")]
    {
        Box::new(MacPermissionChecker)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(FixedPermissionChecker::all_granted())
    }
}

/// Fixed-status checker for tests and for the non-macOS build (where there
/// is no TCC to consult and we want recording to be a no-op rather than a
/// compile error).
pub struct FixedPermissionChecker {
    pub screen_recording: PermissionStatus,
    pub camera: PermissionStatus,
    pub microphone: PermissionStatus,
}

impl FixedPermissionChecker {
    pub fn all_granted() -> Self {
        Self {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Granted,
            microphone: PermissionStatus::Granted,
        }
    }
}

impl PermissionChecker for FixedPermissionChecker {
    fn status(&self, p: Permission) -> PermissionStatus {
        match p {
            Permission::ScreenRecording => self.screen_recording,
            Permission::Camera => self.camera,
            Permission::Microphone => self.microphone,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_captures_each_permission_independently() {
        let checker = FixedPermissionChecker {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::NotDetermined,
        };
        let snap = PermissionsSnapshot::capture(&checker);
        assert_eq!(snap.screen_recording, PermissionStatus::Granted);
        assert_eq!(snap.camera, PermissionStatus::Denied);
        assert_eq!(snap.microphone, PermissionStatus::NotDetermined);
    }

    fn req(role: SourceRole) -> CaptureRequest {
        use crate::core::capture::{CompositionDefaults, Device};
        CaptureRequest {
            role,
            device: Device { id: "default".into(), label: "Default".into() },
            defaults: CompositionDefaults::default(),
            is_transcript_source: false,
        }
    }

    #[test]
    fn satisfies_requests_demands_screen_recording_for_screen_role() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Denied,
            camera: PermissionStatus::Granted,
            microphone: PermissionStatus::Granted,
        };
        assert!(!snap.satisfies_requests(&[req(SourceRole::Screen)]));
    }

    #[test]
    fn satisfies_requests_demands_mic_only_when_microphone_role_present() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::Denied,
        };
        // Screen only — mic permission irrelevant.
        assert!(snap.satisfies_requests(&[req(SourceRole::Screen)]));
        // Screen + Mic — mic permission required.
        assert!(!snap.satisfies_requests(&[
            req(SourceRole::Screen),
            req(SourceRole::Microphone)
        ]));
    }

    #[test]
    fn satisfies_requests_demands_camera_only_when_camera_role_present() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::Granted,
        };
        assert!(snap.satisfies_requests(&[
            req(SourceRole::Screen),
            req(SourceRole::Microphone)
        ]));
        assert!(!snap.satisfies_requests(&[
            req(SourceRole::Screen),
            req(SourceRole::Camera)
        ]));
    }

    #[test]
    fn window_and_system_audio_ride_on_screen_recording_permission() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::Denied,
        };
        assert!(snap.satisfies_requests(&[req(SourceRole::Window)]));
        assert!(snap.satisfies_requests(&[req(SourceRole::SystemAudio)]));
    }

    #[test]
    fn settings_pane_url_is_distinct_per_pane() {
        let urls = [
            SettingsPane::ScreenRecording.url(),
            SettingsPane::Camera.url(),
            SettingsPane::Microphone.url(),
        ];
        let mut sorted = urls.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3, "panes must deep-link to distinct URLs");
    }

    #[test]
    fn settings_pane_for_permission_is_consistent() {
        assert_eq!(
            SettingsPane::for_permission(Permission::ScreenRecording),
            SettingsPane::ScreenRecording
        );
        assert_eq!(SettingsPane::for_permission(Permission::Camera), SettingsPane::Camera);
        assert_eq!(SettingsPane::for_permission(Permission::Microphone), SettingsPane::Microphone);
    }

    /// Manual / on-device check: with Camera permission revoked for this
    /// host process in System Settings → Privacy & Security → Camera, the
    /// TCC bridge must surface `Denied` (not the legacy optimistic
    /// `Granted`). Ignored by default because CI cannot revoke TCC; run
    /// locally after revoking Camera with:
    /// `cargo test -p courseforge -- --ignored mac_revoked_camera_reads_as_denied`
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn mac_revoked_camera_reads_as_denied() {
        let snap = PermissionsSnapshot::capture(&MacPermissionChecker);
        assert_eq!(
            snap.camera,
            PermissionStatus::Denied,
            "expected Camera to read as Denied; revoke Camera in System Settings before running",
        );
    }

    /// Every value `AVAuthorizationStatus` is documented to return must land
    /// in exactly one `PermissionStatus`. Anything else means the recorder
    /// would either spin up against a denied device or refuse a granted one.
    #[cfg(target_os = "macos")]
    #[test]
    fn av_authorization_status_maps_each_variant() {
        use objc2_av_foundation::AVAuthorizationStatus;
        assert_eq!(
            permission_status_from_av(AVAuthorizationStatus::Authorized),
            PermissionStatus::Granted,
        );
        assert_eq!(
            permission_status_from_av(AVAuthorizationStatus::Denied),
            PermissionStatus::Denied,
        );
        assert_eq!(
            permission_status_from_av(AVAuthorizationStatus::NotDetermined),
            PermissionStatus::NotDetermined,
        );
        assert_eq!(
            permission_status_from_av(AVAuthorizationStatus::Restricted),
            PermissionStatus::Restricted,
        );
    }
}
