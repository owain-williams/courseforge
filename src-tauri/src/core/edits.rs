//! Non-destructive edits — the Edit Decision List (EDL).
//!
//! A Video's transcript-driven cuts live on disk at
//! `videos/<video-id>/edits.json`. The source Segment MKVs are never
//! modified (FR-5.4); playback and export read the EDL and skip cut regions
//! at render time.
//!
//! ## Why a command log, not a snapshot
//!
//! The on-disk format is an ordered list of *commands* (`AddCut`, `Undo`,
//! `Redo`) rather than a snapshot of currently-active cuts. That gives us
//! unlimited undo/redo (FR-5.5) for free — replaying the log reconstructs
//! both the active cut set and the redo stack — and it keeps every action
//! the user has ever taken in this session recoverable.
//!
//! ## Crash resilience
//!
//! Each command append rewrites the file atomically via tmp + rename
//! (NFR-5: a crash loses at most the most recent action). The log is small
//! — a typical Video has tens, not thousands, of cuts — so the rewrite cost
//! is negligible.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};

pub const EDITS_FILENAME: &str = "edits.json";
pub const SCHEMA_VERSION: u32 = 1;

/// A single cut: a time range on the Video's timeline that should be skipped
/// during playback and dropped during export. Both bounds are seconds from
/// the start of the Video; `end_sec > start_sec` is required.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Cut {
    pub id: String,
    #[serde(rename = "startSec")]
    pub start_sec: f64,
    #[serde(rename = "endSec")]
    pub end_sec: f64,
}

/// One entry in the persisted command log. Storing this as a tagged enum
/// keeps the on-disk format obvious — every action the user takes shows up
/// as one object with a `kind` field.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Command {
    /// Add a cut. The id is allocated by the caller so the UI can correlate
    /// optimistic state with the persisted entry.
    AddCut {
        id: String,
        #[serde(rename = "startSec")]
        start_sec: f64,
        #[serde(rename = "endSec")]
        end_sec: f64,
    },
    Undo,
    Redo,
}

/// The whole on-disk EDL for one Video — schema version, the Video id (so
/// the file is self-describing if read in isolation), and the command log.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditLog {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "videoId")]
    pub video_id: String,
    pub commands: Vec<Command>,
}

impl EditLog {
    pub fn empty(video_id: impl Into<String>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            video_id: video_id.into(),
            commands: Vec::new(),
        }
    }
}

/// The derived view of an EDL after replaying its command log:
/// the currently-active cuts (sorted by `start_sec`), and whether the next
/// Undo or Redo would do anything useful. The UI consumes this; the log
/// itself is implementation detail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EditState {
    pub cuts: Vec<Cut>,
    #[serde(rename = "canUndo")]
    pub can_undo: bool,
    #[serde(rename = "canRedo")]
    pub can_redo: bool,
}

pub fn edits_path(folder: &Path, video_id: &str) -> PathBuf {
    folder.join("videos").join(video_id).join(EDITS_FILENAME)
}

/// Read the EDL for a Video. Missing file is a normal "no edits yet" state,
/// not an error.
pub fn read_edit_log(folder: &Path, video_id: &str) -> Result<Option<EditLog>> {
    let path = edits_path(folder, video_id);
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = std::fs::read(&path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    let log: EditLog = serde_json::from_slice(&bytes).map_err(|e| CoreError::InvalidEditsJson {
        path: path.clone(),
        source: e,
    })?;
    Ok(Some(log))
}

/// Persist the EDL atomically — write to a sibling `.tmp` then rename, so
/// a crash mid-write can never leave a truncated `edits.json` (NFR-5).
pub fn write_edit_log(folder: &Path, video_id: &str, log: &EditLog) -> Result<()> {
    let path = edits_path(folder, video_id);
    let parent = path.parent().expect("edits_path always has a parent");
    std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
        path: parent.to_path_buf(),
        source: e,
    })?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(log).expect("EditLog serialisation is infallible");
    std::fs::write(&tmp, json).map_err(|e| CoreError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    Ok(())
}

/// Walk the command log and derive the live cut set plus undo/redo
/// capability. Standard editor semantics: a fresh `AddCut` clears the redo
/// stack so undo/redo never crosses a divergent branch.
pub fn replay(log: &EditLog) -> EditState {
    let mut active: Vec<Cut> = Vec::new();
    let mut undone: Vec<Cut> = Vec::new();
    for cmd in &log.commands {
        match cmd {
            Command::AddCut { id, start_sec, end_sec } => {
                undone.clear();
                active.push(Cut {
                    id: id.clone(),
                    start_sec: *start_sec,
                    end_sec: *end_sec,
                });
            }
            Command::Undo => {
                if let Some(c) = active.pop() {
                    undone.push(c);
                }
            }
            Command::Redo => {
                if let Some(c) = undone.pop() {
                    active.push(c);
                }
            }
        }
    }
    let can_undo = !active.is_empty();
    let can_redo = !undone.is_empty();
    active.sort_by(|a, b| {
        a.start_sec
            .partial_cmp(&b.start_sec)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    EditState {
        cuts: active,
        can_undo,
        can_redo,
    }
}

/// Append an `AddCut` to the log, persist, and return the derived state.
/// The cut id is allocated here so different replays of the same log don't
/// drift apart.
pub fn append_cut(
    folder: &Path,
    video_id: &str,
    start_sec: f64,
    end_sec: f64,
) -> Result<EditState> {
    if !(end_sec > start_sec) {
        return Err(CoreError::InvalidCut { start: start_sec, end: end_sec });
    }
    let mut log = read_edit_log(folder, video_id)?
        .unwrap_or_else(|| EditLog::empty(video_id));
    log.commands.push(Command::AddCut {
        id: uuid::Uuid::new_v4().to_string(),
        start_sec,
        end_sec,
    });
    write_edit_log(folder, video_id, &log)?;
    Ok(replay(&log))
}

pub fn append_undo(folder: &Path, video_id: &str) -> Result<EditState> {
    let mut log = read_edit_log(folder, video_id)?
        .unwrap_or_else(|| EditLog::empty(video_id));
    log.commands.push(Command::Undo);
    write_edit_log(folder, video_id, &log)?;
    Ok(replay(&log))
}

pub fn append_redo(folder: &Path, video_id: &str) -> Result<EditState> {
    let mut log = read_edit_log(folder, video_id)?
        .unwrap_or_else(|| EditLog::empty(video_id));
    log.commands.push(Command::Redo);
    write_edit_log(folder, video_id, &log)?;
    Ok(replay(&log))
}

/// Load the log and return its derived state. Missing log folds to the
/// clean state — no cuts, nothing to undo/redo.
pub fn current_state(folder: &Path, video_id: &str) -> Result<EditState> {
    Ok(read_edit_log(folder, video_id)?
        .map(|log| replay(&log))
        .unwrap_or(EditState {
            cuts: Vec::new(),
            can_undo: false,
            can_redo: false,
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course_folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn edits_path_is_under_video_subfolder() {
        let f = course_folder();
        let p = edits_path(f.path(), "vid-1");
        assert_eq!(
            p,
            f.path().join("videos").join("vid-1").join(EDITS_FILENAME)
        );
    }

    #[test]
    fn read_edit_log_returns_none_when_file_missing() {
        let f = course_folder();
        let log = read_edit_log(f.path(), "ghost").unwrap();
        assert!(log.is_none());
    }

    #[test]
    fn write_then_read_round_trips_command_log() {
        let f = course_folder();
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "vid-1".into(),
            commands: vec![
                Command::AddCut { id: "c1".into(), start_sec: 1.0, end_sec: 2.0 },
                Command::Undo,
                Command::Redo,
            ],
        };
        write_edit_log(f.path(), "vid-1", &log).unwrap();
        let got = read_edit_log(f.path(), "vid-1").unwrap().unwrap();
        assert_eq!(got, log);
    }

    #[test]
    fn write_creates_video_subfolder_if_missing() {
        let f = course_folder();
        let log = EditLog::empty("brand-new");
        write_edit_log(f.path(), "brand-new", &log).unwrap();
        assert!(edits_path(f.path(), "brand-new").is_file());
    }

    #[test]
    fn read_errors_clearly_on_malformed_json() {
        let f = course_folder();
        let path = edits_path(f.path(), "vid-broken");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"{ not json }").unwrap();
        let err = read_edit_log(f.path(), "vid-broken").unwrap_err();
        assert!(matches!(err, CoreError::InvalidEditsJson { .. }));
    }

    #[test]
    fn write_is_atomic_tmp_file_is_cleaned_up() {
        // The atomic rename should leave only edits.json behind — no stray
        // edits.json.tmp.
        let f = course_folder();
        let log = EditLog::empty("v");
        write_edit_log(f.path(), "v", &log).unwrap();
        let dir = f.path().join("videos").join("v");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(entries, vec![EDITS_FILENAME.to_string()]);
    }

    #[test]
    fn replay_of_empty_log_yields_no_cuts_and_no_undo_redo() {
        let log = EditLog::empty("v");
        let state = replay(&log);
        assert!(state.cuts.is_empty());
        assert!(!state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn replay_with_one_add_cut_yields_one_active_cut_and_enables_undo() {
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![Command::AddCut {
                id: "c1".into(),
                start_sec: 1.0,
                end_sec: 2.5,
            }],
        };
        let state = replay(&log);
        assert_eq!(state.cuts.len(), 1);
        assert_eq!(state.cuts[0].id, "c1");
        assert!(state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn replay_sorts_active_cuts_by_start_so_the_ui_does_not_have_to() {
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![
                Command::AddCut { id: "b".into(), start_sec: 5.0, end_sec: 6.0 },
                Command::AddCut { id: "a".into(), start_sec: 1.0, end_sec: 2.0 },
                Command::AddCut { id: "c".into(), start_sec: 3.0, end_sec: 4.0 },
            ],
        };
        let state = replay(&log);
        let ids: Vec<_> = state.cuts.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "c", "b"]);
    }

    #[test]
    fn undo_moves_a_cut_to_the_redo_stack_and_flips_flags() {
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![
                Command::AddCut { id: "c1".into(), start_sec: 1.0, end_sec: 2.0 },
                Command::Undo,
            ],
        };
        let state = replay(&log);
        assert!(state.cuts.is_empty());
        assert!(!state.can_undo);
        assert!(state.can_redo);
    }

    #[test]
    fn redo_after_undo_restores_the_cut() {
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![
                Command::AddCut { id: "c1".into(), start_sec: 1.0, end_sec: 2.0 },
                Command::Undo,
                Command::Redo,
            ],
        };
        let state = replay(&log);
        assert_eq!(state.cuts.len(), 1);
        assert!(state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn undo_on_empty_active_stack_is_a_no_op() {
        // Replayer must be defensive: a Undo with nothing to undo (e.g. the
        // user spam-pressed cmd-Z) cannot crash.
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![Command::Undo, Command::Undo],
        };
        let state = replay(&log);
        assert!(state.cuts.is_empty());
        assert!(!state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn redo_on_empty_undone_stack_is_a_no_op() {
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![Command::Redo],
        };
        let state = replay(&log);
        assert!(state.cuts.is_empty());
        assert!(!state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn new_add_cut_after_undo_clears_the_redo_stack() {
        // Standard editor undo: branching kills the redo branch.
        let log = EditLog {
            schema_version: SCHEMA_VERSION,
            video_id: "v".into(),
            commands: vec![
                Command::AddCut { id: "c1".into(), start_sec: 1.0, end_sec: 2.0 },
                Command::Undo,
                Command::AddCut { id: "c2".into(), start_sec: 3.0, end_sec: 4.0 },
            ],
        };
        let state = replay(&log);
        assert_eq!(state.cuts.len(), 1);
        assert_eq!(state.cuts[0].id, "c2");
        assert!(state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn append_cut_persists_and_returns_updated_state() {
        let f = course_folder();
        let state = append_cut(f.path(), "v", 1.0, 2.5).unwrap();
        assert_eq!(state.cuts.len(), 1);
        assert!(state.can_undo);

        // Persisted log has exactly one AddCut.
        let log = read_edit_log(f.path(), "v").unwrap().unwrap();
        assert_eq!(log.commands.len(), 1);
        assert!(matches!(log.commands[0], Command::AddCut { .. }));
    }

    #[test]
    fn append_cut_rejects_inverted_or_empty_ranges() {
        let f = course_folder();
        let err = append_cut(f.path(), "v", 2.0, 2.0).unwrap_err();
        assert!(matches!(err, CoreError::InvalidCut { .. }));
        let err = append_cut(f.path(), "v", 3.0, 1.0).unwrap_err();
        assert!(matches!(err, CoreError::InvalidCut { .. }));
        // No log should have been written for the rejected attempts.
        assert!(read_edit_log(f.path(), "v").unwrap().is_none());
    }

    #[test]
    fn append_cut_undo_redo_round_trip_through_disk() {
        // Full integration of the persistence + replay path: each action
        // hits disk, and `current_state` after relaunch sees the same view.
        let f = course_folder();
        let s1 = append_cut(f.path(), "v", 1.0, 2.0).unwrap();
        assert_eq!(s1.cuts.len(), 1);

        let s2 = append_cut(f.path(), "v", 5.0, 6.0).unwrap();
        assert_eq!(s2.cuts.len(), 2);

        let s3 = append_undo(f.path(), "v").unwrap();
        assert_eq!(s3.cuts.len(), 1);
        assert!(s3.can_redo);

        let s4 = append_redo(f.path(), "v").unwrap();
        assert_eq!(s4.cuts.len(), 2);

        // Simulate relaunch — re-read from disk.
        let reloaded = current_state(f.path(), "v").unwrap();
        assert_eq!(reloaded, s4);
    }

    #[test]
    fn current_state_with_no_log_is_the_clean_state() {
        let f = course_folder();
        let state = current_state(f.path(), "v-fresh").unwrap();
        assert!(state.cuts.is_empty());
        assert!(!state.can_undo);
        assert!(!state.can_redo);
    }

    #[test]
    fn unlimited_undo_redo_within_a_long_session() {
        // FR-5.5: undo/redo are unlimited. Drive a couple of hundred adds
        // followed by a couple of hundred undos and a couple of hundred
        // redos; we should land back where we started.
        let f = course_folder();
        for i in 0..200 {
            append_cut(f.path(), "v", i as f64, (i as f64) + 0.5).unwrap();
        }
        assert_eq!(current_state(f.path(), "v").unwrap().cuts.len(), 200);

        for _ in 0..200 {
            append_undo(f.path(), "v").unwrap();
        }
        assert!(current_state(f.path(), "v").unwrap().cuts.is_empty());

        for _ in 0..200 {
            append_redo(f.path(), "v").unwrap();
        }
        assert_eq!(current_state(f.path(), "v").unwrap().cuts.len(), 200);
    }

    #[test]
    fn schema_version_is_persisted_so_we_can_migrate_later() {
        let f = course_folder();
        append_cut(f.path(), "v", 0.0, 1.0).unwrap();
        let raw = std::fs::read_to_string(edits_path(f.path(), "v")).unwrap();
        assert!(raw.contains("\"schemaVersion\""));
        assert!(raw.contains(&format!("\"schemaVersion\": {SCHEMA_VERSION}")));
    }
}
