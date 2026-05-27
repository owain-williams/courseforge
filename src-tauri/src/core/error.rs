use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid course.json at {path}: {source}")]
    InvalidCourseJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid transcript.json at {path}: {source}")]
    InvalidTranscriptJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid edits.json at {path}: {source}")]
    InvalidEditsJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("invalid scenes.json at {path}: {source}")]
    InvalidScenesJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("scene not found: {0}")]
    SceneNotFound(String),

    #[error("scene source index {index} out of bounds for scene {scene_id} (len {len})")]
    SceneSourceIndexOutOfBounds {
        scene_id: String,
        index: usize,
        len: usize,
    },

    #[error("scene \"{scene_name}\" has no sources to record")]
    SceneHasNoSources { scene_name: String },

    #[error(
        "scene \"{scene_name}\" expects {role} device \"{device_label}\" ({device_id}) but it's not currently attached"
    )]
    SceneDeviceMissing {
        scene_name: String,
        role: String,
        device_label: String,
        device_id: String,
    },

    #[error("invalid cut: end ({end}) must be greater than start ({start})")]
    InvalidCut { start: f64, end: f64 },

    #[error("title cannot be empty")]
    EmptyTitle,

    #[error("not a course folder: {0}")]
    NotACourseFolder(PathBuf),

    #[error("module not found: {0}")]
    ModuleNotFound(String),

    #[error("video not found: {0}")]
    VideoNotFound(String),

    #[error("reorder list does not match existing items")]
    ReorderMismatch,

    #[error("workflow state not found: {0}")]
    WorkflowStateNotFound(String),

    #[error("cannot remove the last remaining workflow state")]
    OnlyWorkflowStateLeft,

    #[error("failed to move {path} to Trash: {message}")]
    Trash { path: PathBuf, message: String },

    #[error("segment not found: {0}")]
    SegmentNotFound(String),

    #[error("recording session not found: {0}")]
    SessionNotFound(String),

    #[error("cannot {action} a session in state {state}")]
    InvalidSessionTransition { action: String, state: String },

    #[error("recorder backend failed: {0}")]
    Recorder(String),

    #[error("transcriber backend failed: {0}")]
    Transcriber(String),

    #[error("no segments available to transcribe for video: {0}")]
    NoSegmentsForTranscription(String),

    #[error("transcription job not found: {0}")]
    TranscriptionJobNotFound(String),

    #[error("exporter backend failed: {0}")]
    Exporter(String),

    #[error("export cancelled by user")]
    ExportCancelled,

    #[error("export job not found: {0}")]
    ExportJobNotFound(String),

    #[error("export needs at least one Segment for video: {0}")]
    NoSegmentsForExport(String),

    #[error("a transcript is required to export captions for video: {0}")]
    NoTranscriptForExport(String),

    #[error("cannot determine duration of {path}: {message}")]
    DurationUnknown { path: PathBuf, message: String },

    #[error("an export is already in progress for video: {0}")]
    ExportAlreadyRunning(String),
}

pub type Result<T> = std::result::Result<T, CoreError>;
