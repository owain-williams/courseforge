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
}

pub type Result<T> = std::result::Result<T, CoreError>;
