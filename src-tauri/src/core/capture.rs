//! Public capture-API types shared by the recorder backend, the
//! per-segment sidecar JSON, and the IPC contract with the frontend.
//!
//! Per ADR-0002, a Take is a `Vec<CaptureRequest>` — each entry naming a
//! Source Role bound to a specific device, with composition defaults that
//! describe where the source should sit on the editor canvas. Phase 1
//! exercises the recorder with one or two entries (screen plus optional
//! microphone, matching v1 behaviour); Phase 2 grows it to N.

use serde::{Deserialize, Serialize};

/// Closed taxonomy of capture sources. See ADR-0002.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceRole {
    Screen,
    Window,
    Camera,
    Microphone,
    SystemAudio,
}

impl SourceRole {
    /// True iff the role produces audio samples only (no video track). Used
    /// by the recorder to pick the right output container — audio-only
    /// sources land in `.m4a`, video-and-audio sources in `.mov`.
    pub fn is_audio_only(self) -> bool {
        matches!(self, SourceRole::Microphone | SourceRole::SystemAudio)
    }

    /// True iff the role is sourced from ScreenCaptureKit (screen, window,
    /// system audio). The Phase 2 backend routes these through one
    /// `SCStream` per source. Camera and Microphone go through
    /// `AVCaptureSession` instead.
    pub fn is_sck_sourced(self) -> bool {
        matches!(
            self,
            SourceRole::Screen | SourceRole::Window | SourceRole::SystemAudio
        )
    }
}

/// A specific device the user picked to back a [`SourceRole`]. The id is
/// opaque to this layer — for screens it's a `CGDirectDisplayID` stringified;
/// for AVCaptureSession devices it's the device's uniqueID; for "default"
/// selections the id is the sentinel string `"default"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub label: String,
}

/// Editor composition properties for a source. Canvas-relative, in the
/// frame of the Course's output canvas. `scale = 1.0` means "fill the
/// canvas at the source's aspect ratio". `audio_gain_db` is stored in dB;
/// `f64::NEG_INFINITY` means muted.
///
/// Phase 1 doesn't render these — they're snapshotted into the sidecar so
/// Phase 2+ can use them when laying out Clips.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompositionDefaults {
    #[serde(default)]
    pub position: Position,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(rename = "audioGainDb", default = "default_audio_gain")]
    pub audio_gain_db: f64,
}

fn default_scale() -> f64 {
    1.0
}
fn default_opacity() -> f64 {
    1.0
}
fn default_audio_gain() -> f64 {
    0.0
}

impl Default for CompositionDefaults {
    fn default() -> Self {
        Self {
            position: Position::default(),
            scale: default_scale(),
            opacity: default_opacity(),
            audio_gain_db: default_audio_gain(),
        }
    }
}

/// Top-left position of a source within the canvas, in normalised
/// coordinates (0.0 = left/top edge, 1.0 = right/bottom edge).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Position {
    #[serde(default)]
    pub x: f64,
    #[serde(default)]
    pub y: f64,
}

/// One source to capture in a Take. The recorder receives a `Vec` of these
/// and is responsible for spinning up the underlying OS pipelines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    pub role: SourceRole,
    pub device: Device,
    #[serde(default)]
    pub defaults: CompositionDefaults,
    /// Issue #40 — whether this source is the Scene's designated
    /// Transcript Source. Defaults to `false` so v1 Course Folders and
    /// pre-Phase-2 capture flows stay byte-identical. Phase 6 uses this
    /// flag to filter which audio Segments get sent to the transcriber.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_transcript_source: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// How a Segment's recording ended. Written into the sidecar JSON so the
/// editor can tell a clean stop from a mid-Take device disconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EndedReason {
    /// User clicked Stop (or finished a clean Take).
    Normal,
    /// The source's device disappeared / errored mid-Take. The other
    /// sources in the Take kept going.
    SourceFailed,
    /// The app or OS crashed mid-Take and we recovered the partial file
    /// from disk on next launch.
    Crashed,
}

/// Per-Segment metadata written next to the `.mov` as `<segmentId>.json`.
/// Source of truth for take-grouping (Phase 2 orphan recovery groups by
/// `takeId`), for the source's identity and device, and for the editor's
/// initial composition properties.
///
/// `schemaVersion` is fixed at 1 for Phase 1; future shape changes bump it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentSidecar {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub take_id: String,
    pub source_role: SourceRole,
    pub device: Device,
    /// ISO-8601 UTC timestamp the recording started. String rather than a
    /// chrono / time type because the rest of this crate already deals in
    /// plain strings and adding a date crate for one field is overkill.
    pub recorded_at: String,
    pub defaults: CompositionDefaults,
    pub ended_reason: EndedReason,
    /// Issue #40 — was this Segment captured from the Scene's Transcript
    /// Source slot? Snapshotted at Capture time so Phase 6's transcriber
    /// filter can decide without re-loading the Scene. Defaults to
    /// `false` (omitted from JSON) so v1 Course Folders stay
    /// byte-identical.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_transcript_source: bool,
    /// ISO-8601 UTC timestamp the source's recording ended. Issue #37
    /// records this on mid-Take per-source failure so the editor (and the
    /// user) can tell when one source dropped while the rest of the Take
    /// kept going. `None` is the v1-compatible "we didn't track it" state
    /// — readers should fall back to the file's mtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<String>,
}

impl SegmentSidecar {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn new(
        take_id: impl Into<String>,
        source_role: SourceRole,
        device: Device,
        recorded_at: impl Into<String>,
        defaults: CompositionDefaults,
        ended_reason: EndedReason,
    ) -> Self {
        Self {
            schema_version: Self::SCHEMA_VERSION,
            take_id: take_id.into(),
            source_role,
            device,
            recorded_at: recorded_at.into(),
            defaults,
            ended_reason,
            is_transcript_source: false,
            ended_at: None,
        }
    }

    /// Tag the sidecar as the Take's Transcript Source slot — Phase 6's
    /// filter reads this back instead of re-loading the Scene.
    pub fn with_transcript_source(mut self, flag: bool) -> Self {
        self.is_transcript_source = flag;
        self
    }

    /// Tag the sidecar with an explicit end timestamp — used for mid-Take
    /// per-source failures (issue #37) so the editor can show "this source
    /// dropped at 12:34:56" vs the Take's full duration.
    pub fn with_ended_at(mut self, ended_at: impl Into<String>) -> Self {
        self.ended_at = Some(ended_at.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_role_round_trips_as_camel_case() {
        let cases = [
            (SourceRole::Screen, "\"screen\""),
            (SourceRole::Window, "\"window\""),
            (SourceRole::Camera, "\"camera\""),
            (SourceRole::Microphone, "\"microphone\""),
            (SourceRole::SystemAudio, "\"systemAudio\""),
        ];
        for (role, expected) in cases {
            assert_eq!(serde_json::to_string(&role).unwrap(), expected);
            let parsed: SourceRole = serde_json::from_str(expected).unwrap();
            assert_eq!(parsed, role);
        }
    }

    #[test]
    fn ended_reason_round_trips_as_camel_case() {
        let cases = [
            (EndedReason::Normal, "\"normal\""),
            (EndedReason::SourceFailed, "\"sourceFailed\""),
            (EndedReason::Crashed, "\"crashed\""),
        ];
        for (reason, expected) in cases {
            assert_eq!(serde_json::to_string(&reason).unwrap(), expected);
            let parsed: EndedReason = serde_json::from_str(expected).unwrap();
            assert_eq!(parsed, reason);
        }
    }

    #[test]
    fn composition_defaults_default_is_canvas_filling_unmuted() {
        let d = CompositionDefaults::default();
        assert_eq!(d.position.x, 0.0);
        assert_eq!(d.position.y, 0.0);
        assert_eq!(d.scale, 1.0);
        assert_eq!(d.opacity, 1.0);
        assert_eq!(d.audio_gain_db, 0.0);
    }

    #[test]
    fn composition_defaults_missing_fields_fill_with_default_on_deserialize() {
        // Empty object: every field optional.
        let d: CompositionDefaults = serde_json::from_str("{}").unwrap();
        assert_eq!(d, CompositionDefaults::default());

        // Partial: scale specified, the rest default.
        let d: CompositionDefaults = serde_json::from_str(r#"{"scale": 0.5}"#).unwrap();
        assert_eq!(d.scale, 0.5);
        assert_eq!(d.opacity, 1.0);
    }

    #[test]
    fn composition_defaults_use_audio_gain_db_field_name() {
        let json = serde_json::to_string(&CompositionDefaults::default()).unwrap();
        assert!(json.contains("audioGainDb"), "json was {json}");
        assert!(!json.contains("audio_gain_db"), "json was {json}");
    }

    #[test]
    fn segment_sidecar_round_trips_with_camel_case_field_names() {
        let s = SegmentSidecar::new(
            "take-abc",
            SourceRole::Screen,
            Device {
                id: "default".into(),
                label: "Main Display".into(),
            },
            "2026-05-26T12:00:00Z",
            CompositionDefaults::default(),
            EndedReason::Normal,
        );
        let json = serde_json::to_string(&s).unwrap();
        // Snake-case field names must NOT leak — the on-disk contract is
        // camelCase to match the rest of the JSON files this app writes.
        assert!(json.contains("\"schemaVersion\":1"));
        assert!(json.contains("\"takeId\":\"take-abc\""));
        assert!(json.contains("\"sourceRole\":\"screen\""));
        assert!(json.contains("\"recordedAt\":\"2026-05-26T12:00:00Z\""));
        assert!(json.contains("\"endedReason\":\"normal\""));
        assert!(!json.contains("schema_version"));
        assert!(!json.contains("take_id"));

        let back: SegmentSidecar = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn capture_request_round_trips_with_camel_case_fields() {
        let req = CaptureRequest {
            role: SourceRole::Screen,
            device: Device {
                id: "default".into(),
                label: "Main Display".into(),
            },
            defaults: CompositionDefaults::default(),
            is_transcript_source: false,
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CaptureRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back, req);
        // Sanity check the field names the IPC contract relies on.
        assert!(json.contains("\"role\":\"screen\""));
        assert!(json.contains("\"device\""));
        assert!(json.contains("\"defaults\""));
    }
}
