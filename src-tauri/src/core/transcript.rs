//! Transcripts are the textual mirror of a Video's audio with word-level
//! timestamps. They live on disk at `videos/<video-id>/transcript.json` so
//! they travel with the Course Folder (NFR-6) and can be regenerated, edited,
//! or rendered into captions without re-running ASR.
//!
//! Like Segments, transcripts are *discovered* by reading the file — there is
//! no transcript[] array in `course.json` that could drift out of sync.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};

pub const TRANSCRIPT_FILENAME: &str = "transcript.json";
pub const SCHEMA_VERSION: u32 = 1;

/// A single word with its in-video timing. `start` and `end` are seconds from
/// the start of the Video timeline. `text` is the rendered surface form
/// (including any leading whitespace/punctuation the ASR engine emitted).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Word {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// The full transcript for one Video. `segment_ids` records which Segments
/// contributed; until multi-Segment Videos arrive (issue #11) this is a
/// single id.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "videoId")]
    pub video_id: String,
    #[serde(rename = "segmentIds")]
    pub segment_ids: Vec<String>,
    pub words: Vec<Word>,
}

impl Transcript {
    pub fn new(video_id: String, segment_ids: Vec<String>, words: Vec<Word>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            video_id,
            segment_ids,
            words,
        }
    }
}

pub fn transcript_path(folder: &Path, video_id: &str) -> PathBuf {
    folder.join("videos").join(video_id).join(TRANSCRIPT_FILENAME)
}

pub fn write_transcript(folder: &Path, video_id: &str, t: &Transcript) -> Result<()> {
    let path = transcript_path(folder, video_id);
    let parent = path.parent().expect("transcript_path always has a parent");
    std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;
    let json = serde_json::to_vec_pretty(t).expect("Transcript serialisation is infallible");
    std::fs::write(&path, json).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    Ok(())
}

pub fn read_transcript(folder: &Path, video_id: &str) -> Result<Option<Transcript>> {
    let path = transcript_path(folder, video_id);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    let t: Transcript = serde_json::from_slice(&bytes)
        .map_err(|e| CoreError::InvalidTranscriptJson {
            path: path.clone(),
            source: e,
        })?;
    Ok(Some(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course_folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn transcript_path_is_under_video_subfolder() {
        let f = course_folder();
        let p = transcript_path(f.path(), "vid-1");
        assert_eq!(
            p,
            f.path().join("videos").join("vid-1").join(TRANSCRIPT_FILENAME)
        );
    }

    #[test]
    fn read_transcript_returns_none_when_file_missing() {
        let f = course_folder();
        let t = read_transcript(f.path(), "vid-ghost").unwrap();
        assert!(t.is_none());
    }

    #[test]
    fn write_then_read_round_trips_word_timestamps() {
        let f = course_folder();
        let t = Transcript::new(
            "vid-1".into(),
            vec!["seg-a".into()],
            vec![
                Word { start: 0.0, end: 0.32, text: "Hello".into() },
                Word { start: 0.32, end: 0.91, text: " world".into() },
            ],
        );
        write_transcript(f.path(), "vid-1", &t).unwrap();
        let got = read_transcript(f.path(), "vid-1").unwrap().expect("file should exist");
        assert_eq!(got, t);
    }

    #[test]
    fn write_transcript_creates_the_video_subfolder_if_missing() {
        let f = course_folder();
        let t = Transcript::new("brand-new-vid".into(), vec![], vec![]);
        write_transcript(f.path(), "brand-new-vid", &t).unwrap();
        assert!(transcript_path(f.path(), "brand-new-vid").is_file());
    }

    #[test]
    fn read_transcript_errors_clearly_on_malformed_json() {
        let f = course_folder();
        let path = transcript_path(f.path(), "vid-broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json }").unwrap();

        let err = read_transcript(f.path(), "vid-broken").unwrap_err();
        assert!(matches!(err, CoreError::InvalidTranscriptJson { .. }));
    }

    #[test]
    fn schema_version_is_recorded_in_the_serialised_payload() {
        let f = course_folder();
        let t = Transcript::new("v".into(), vec![], vec![]);
        write_transcript(f.path(), "v", &t).unwrap();
        let raw = std::fs::read_to_string(transcript_path(f.path(), "v")).unwrap();
        assert!(raw.contains("\"schemaVersion\""));
        assert!(raw.contains(&format!("\"schemaVersion\": {SCHEMA_VERSION}")));
    }
}
