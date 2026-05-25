//! macOS capture permissions, abstracted so the rest of the app can ask
//! "may we record?" without caring whether we're on macOS or a test rig.
//!
//! TCC (Apple's privacy framework) does not let an app *grant* permission
//! programmatically — only the user, in System Settings, can do that. The
//! best we can do is detect status and deep-link the user to the right pane.

use serde::{Deserialize, Serialize};

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

    /// True iff every permission required for the requested sources is granted.
    /// Screen recording is always required (v1 always captures the display);
    /// mic / camera only if the user enabled them.
    pub fn satisfies(&self, sources: &CaptureSources) -> bool {
        if !self.screen_recording.is_granted() {
            return false;
        }
        if sources.microphone && !self.microphone.is_granted() {
            return false;
        }
        if sources.webcam && !self.camera.is_granted() {
            return false;
        }
        true
    }
}

/// What the user asked to capture for a session. v1 always captures the main
/// display; mic / webcam / system-audio are opt-in. System audio and webcam
/// are wired through the type but not yet implemented end-to-end (deferred).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureSources {
    pub microphone: bool,
    #[serde(rename = "systemAudio", default)]
    pub system_audio: bool,
    #[serde(default)]
    pub webcam: bool,
}

impl Default for CaptureSources {
    fn default() -> Self {
        Self { microphone: true, system_audio: false, webcam: false }
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
/// granted, *without* prompting). Camera and Microphone status are not yet
/// wired through — we report `Granted` optimistically and let the recorder
/// surface a permission error if avfoundation refuses to open the device.
/// The full TCC query for AV devices needs an ObjC bridge and is deliberately
/// deferred to a follow-up alongside the ScreenCaptureKit migration.
#[cfg(target_os = "macos")]
pub struct MacPermissionChecker;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
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
            // See doc comment above — optimistic until we bridge ObjC.
            Permission::Camera | Permission::Microphone => PermissionStatus::Granted,
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

    #[test]
    fn satisfies_requires_screen_recording_unconditionally() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Denied,
            camera: PermissionStatus::Granted,
            microphone: PermissionStatus::Granted,
        };
        let sources = CaptureSources { microphone: false, system_audio: false, webcam: false };
        assert!(!snap.satisfies(&sources));
    }

    #[test]
    fn satisfies_only_demands_mic_when_microphone_source_requested() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::Denied,
        };
        // Mic off → mic permission doesn't matter.
        let no_mic = CaptureSources { microphone: false, system_audio: false, webcam: false };
        assert!(snap.satisfies(&no_mic));
        // Mic on → mic permission is required.
        let with_mic = CaptureSources { microphone: true, system_audio: false, webcam: false };
        assert!(!snap.satisfies(&with_mic));
    }

    #[test]
    fn satisfies_only_demands_camera_when_webcam_source_requested() {
        let snap = PermissionsSnapshot {
            screen_recording: PermissionStatus::Granted,
            camera: PermissionStatus::Denied,
            microphone: PermissionStatus::Granted,
        };
        let no_cam = CaptureSources { microphone: true, system_audio: false, webcam: false };
        assert!(snap.satisfies(&no_cam));
        let with_cam = CaptureSources { microphone: true, system_audio: false, webcam: true };
        assert!(!snap.satisfies(&with_cam));
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
}
