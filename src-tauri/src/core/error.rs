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
}

pub type Result<T> = std::result::Result<T, CoreError>;
