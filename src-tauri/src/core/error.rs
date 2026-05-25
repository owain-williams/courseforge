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
}

pub type Result<T> = std::result::Result<T, CoreError>;
