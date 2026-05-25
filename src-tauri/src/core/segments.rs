//! Segments are the raw recorded captures that belong to a Video.
//!
//! On disk, per ADR-0001 and CONTEXT.md, downstream per-Video state lives in
//! per-Video subfolders rather than inside `course.json`. Segments follow that
//! rule: they sit at `videos/<video-id>/segments/<segment-id>.mkv`, and the
//! Library / Course view discovers them by scanning the folder — there is no
//! segments[] array in `course.json` that could drift out of sync.
//!
//! During capture a segment is written as `<id>.partial.mkv`. On a clean Stop
//! the user is asked Keep / Discard; Keep renames it to `<id>.mkv` (the marker
//! that promotes it to a real Segment), Discard removes it. If the app or OS
//! crashes mid-capture the `.partial.mkv` is left behind; on next launch we
//! offer those orphans back to the user as importable Segments.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};

pub const SEGMENT_EXT: &str = "mkv";
pub const PARTIAL_SUFFIX: &str = "partial.mkv";

/// A finalised Segment on disk. Discovered by scanning, not stored in course.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Segment {
    pub id: String,
    #[serde(rename = "videoId")]
    pub video_id: String,
    /// Path relative to the Course Folder (e.g. `videos/<vid>/segments/<sid>.mkv`).
    /// Relative so a Course Folder copied to another Mac still resolves.
    pub path: PathBuf,
}

/// An in-progress segment file that survived a crash. Same shape as `Segment`
/// but the path points at the `.partial.mkv` so the UI / caller knows to offer
/// import-or-discard rather than treating it as a finished take.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrphanSegment {
    pub id: String,
    #[serde(rename = "videoId")]
    pub video_id: String,
    pub path: PathBuf,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn segments_dir(folder: &Path, video_id: &str) -> PathBuf {
    folder.join("videos").join(video_id).join("segments")
}

/// Allocate a fresh Segment id and the absolute path the recorder should
/// stream the `.partial.mkv` to. The segments folder is created if missing.
/// Returns the absolute path so the recorder doesn't have to know about the
/// Course Folder layout — callers serialise it back to a relative path
/// before persisting (see `to_relative`).
pub fn prepare_segment_path(folder: &Path, video_id: &str) -> Result<(String, PathBuf)> {
    let dir = segments_dir(folder, video_id);
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let id = new_id();
    let path = dir.join(format!("{id}.{PARTIAL_SUFFIX}"));
    Ok((id, path))
}

/// Rename `<id>.partial.mkv` → `<id>.mkv`, promoting an in-progress capture
/// to a real Segment. Idempotent-ish: if `.partial.mkv` is missing but the
/// final `.mkv` already exists we treat that as already-finalised and return
/// it; otherwise we error.
pub fn finalize_segment(folder: &Path, video_id: &str, segment_id: &str) -> Result<Segment> {
    let dir = segments_dir(folder, video_id);
    let partial = dir.join(format!("{segment_id}.{PARTIAL_SUFFIX}"));
    let final_path = dir.join(format!("{segment_id}.{SEGMENT_EXT}"));

    if partial.is_file() {
        std::fs::rename(&partial, &final_path).map_err(|e| CoreError::Io {
            path: final_path.clone(),
            source: e,
        })?;
    } else if !final_path.is_file() {
        return Err(CoreError::SegmentNotFound(segment_id.to_string()));
    }

    let rel = to_relative(folder, &final_path);
    Ok(Segment {
        id: segment_id.to_string(),
        video_id: video_id.to_string(),
        path: rel,
    })
}

/// Delete the in-progress `.partial.mkv` for a segment. Missing files are
/// treated as success — Discard is meant to be safe to call after a crash
/// or after the user already cleaned up by hand.
pub fn discard_partial(folder: &Path, video_id: &str, segment_id: &str) -> Result<()> {
    let partial = segments_dir(folder, video_id).join(format!("{segment_id}.{PARTIAL_SUFFIX}"));
    if partial.is_file() {
        std::fs::remove_file(&partial).map_err(|e| CoreError::Io {
            path: partial,
            source: e,
        })?;
    }
    Ok(())
}

/// List finalised Segments for a Video by scanning its segments folder.
/// Hidden files, the `.partial.mkv` workfiles and anything that isn't a plain
/// `<id>.mkv` are skipped. Order is by filename so the result is stable for
/// tests and for UI rendering.
pub fn list_segments(folder: &Path, video_id: &str) -> Result<Vec<Segment>> {
    let dir = segments_dir(folder, video_id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| CoreError::Io {
        path: dir.clone(),
        source: e,
    })? {
        let entry = entry.map_err(|e| CoreError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') || name.ends_with(&format!(".{PARTIAL_SUFFIX}")) {
            continue;
        }
        let id = match name.strip_suffix(&format!(".{SEGMENT_EXT}")) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        out.push(Segment {
            id,
            video_id: video_id.to_string(),
            path: to_relative(folder, &path),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Find every `*.partial.mkv` under any Video's segments folder. Called on
/// Course window open so the user can be offered the chance to import or
/// discard captures that didn't survive a clean Stop.
pub fn scan_orphans(folder: &Path) -> Result<Vec<OrphanSegment>> {
    let videos_root = folder.join("videos");
    if !videos_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for video_entry in std::fs::read_dir(&videos_root).map_err(|e| CoreError::Io {
        path: videos_root.clone(),
        source: e,
    })? {
        let video_entry = match video_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !video_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let video_id = match video_entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let segs = video_entry.path().join("segments");
        if !segs.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&segs).map_err(|e| CoreError::Io {
            path: segs.clone(),
            source: e,
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            let id = match name.strip_suffix(&format!(".{PARTIAL_SUFFIX}")) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            out.push(OrphanSegment {
                id,
                video_id: video_id.clone(),
                path: to_relative(folder, &path),
            });
        }
    }
    out.sort_by(|a, b| (a.video_id.as_str(), a.id.as_str()).cmp(&(&b.video_id, &b.id)));
    Ok(out)
}

fn to_relative(folder: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(folder).map(PathBuf::from).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn course_folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn prepare_segment_path_creates_segments_dir_and_returns_partial_under_it() {
        let f = course_folder();
        let (id, path) = prepare_segment_path(f.path(), "vid-1").unwrap();

        assert!(!id.is_empty());
        let expected_dir = f.path().join("videos").join("vid-1").join("segments");
        assert!(expected_dir.is_dir(), "segments dir was not created");
        assert_eq!(path.parent().unwrap(), expected_dir);

        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, format!("{id}.{PARTIAL_SUFFIX}"));
    }

    #[test]
    fn prepare_segment_path_gives_distinct_ids_for_successive_calls() {
        let f = course_folder();
        let (a, _) = prepare_segment_path(f.path(), "vid-1").unwrap();
        let (b, _) = prepare_segment_path(f.path(), "vid-1").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn finalize_segment_renames_partial_to_final_and_returns_relative_path() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"FAKE-MKV").unwrap();

        let seg = finalize_segment(f.path(), "vid-1", &id).unwrap();
        assert_eq!(seg.id, id);
        assert_eq!(seg.video_id, "vid-1");
        assert_eq!(
            seg.path,
            PathBuf::from("videos").join("vid-1").join("segments").join(format!("{id}.{SEGMENT_EXT}"))
        );

        assert!(!partial.exists(), "partial should be gone");
        assert!(f.path().join(&seg.path).is_file(), "final .mkv should exist");
    }

    #[test]
    fn finalize_segment_is_idempotent_if_already_finalised() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id).unwrap();

        // Calling again with the partial gone but the final present should
        // not fail — Keep is a user-driven action and we don't want
        // double-clicks or replays to error.
        let seg = finalize_segment(f.path(), "vid-1", &id).unwrap();
        assert_eq!(seg.id, id);
    }

    #[test]
    fn finalize_segment_errors_when_neither_partial_nor_final_exists() {
        let f = course_folder();
        let result = finalize_segment(f.path(), "vid-1", "no-such-segment");
        assert!(matches!(result, Err(CoreError::SegmentNotFound(_))));
    }

    #[test]
    fn discard_partial_removes_the_partial_file() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();

        discard_partial(f.path(), "vid-1", &id).unwrap();
        assert!(!partial.exists());
    }

    #[test]
    fn discard_partial_is_silent_when_file_is_already_missing() {
        let f = course_folder();
        // Pretend a crash already cleaned things up — Discard should still
        // succeed so the UI flow stays simple.
        discard_partial(f.path(), "vid-1", "ghost").unwrap();
    }

    #[test]
    fn discard_partial_does_not_touch_finalised_segments() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id).unwrap();

        discard_partial(f.path(), "vid-1", &id).unwrap();
        let final_path = f.path().join("videos").join("vid-1").join("segments").join(format!("{id}.{SEGMENT_EXT}"));
        assert!(final_path.is_file(), "finalised file must be preserved");
    }

    #[test]
    fn list_segments_returns_empty_when_segments_dir_missing() {
        let f = course_folder();
        let segs = list_segments(f.path(), "vid-1").unwrap();
        assert!(segs.is_empty());
    }

    #[test]
    fn list_segments_includes_finals_and_skips_partials_and_dotfiles() {
        let f = course_folder();
        let (id_keep, p_keep) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_keep, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id_keep).unwrap();

        // A still-in-progress one and a dotfile that should be ignored.
        let (_, p_partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_partial, b"x").unwrap();
        std::fs::write(
            f.path().join("videos").join("vid-1").join("segments").join(".DS_Store"),
            b"x",
        )
        .unwrap();

        let segs = list_segments(f.path(), "vid-1").unwrap();
        let ids: Vec<_> = segs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec![id_keep.as_str()]);
    }

    #[test]
    fn scan_orphans_finds_partials_across_all_video_folders() {
        let f = course_folder();
        let (id_a, pa) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&pa, b"x").unwrap();
        let (id_b, pb) = prepare_segment_path(f.path(), "vid-2").unwrap();
        std::fs::write(&pb, b"x").unwrap();

        // A finalised one in vid-1 — should NOT show up as an orphan.
        let (id_c, pc) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&pc, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id_c).unwrap();

        let mut orphans = scan_orphans(f.path()).unwrap();
        orphans.sort_by(|a, b| a.id.cmp(&b.id));
        let mut expected = vec![id_a.clone(), id_b.clone()];
        expected.sort();
        let got: Vec<_> = orphans.iter().map(|o| o.id.clone()).collect();
        assert_eq!(got, expected);

        // Each orphan path is relative to the course folder and points at a
        // .partial.mkv file that still exists.
        for o in &orphans {
            assert!(o.path.starts_with("videos"));
            assert!(f.path().join(&o.path).is_file());
            assert!(o.path.to_string_lossy().ends_with(PARTIAL_SUFFIX));
        }
    }

    #[test]
    fn scan_orphans_returns_empty_when_videos_dir_absent() {
        let f = course_folder();
        let orphans = scan_orphans(f.path()).unwrap();
        assert!(orphans.is_empty());
    }
}
