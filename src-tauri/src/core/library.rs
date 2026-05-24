use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use serde::{Deserialize, Serialize};
use crate::core::config::AppConfig;
use crate::core::course::{read_course, COURSE_JSON};
use crate::core::error::{CoreError, Result};

/// Where a Course in the Library came from. Visually indistinguishable in the
/// UI (ADR-0001), but the back-end needs the distinction so "Remove from
/// Library" on a Pinned Folder unpins vs. only being offered alongside Move
/// to Trash for Scanned-Root entries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CourseSource {
    Scanned,
    Pinned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseEntry {
    pub folder: PathBuf,
    pub title: String,
    /// Last-modified time of the Course Folder, as Unix milliseconds.
    pub modified_ms: i64,
    pub video_count: usize,
    pub source: CourseSource,
    /// Pinned Folder that wasn't found on disk at scan time. The entry is
    /// kept (not auto-removed) so the user can decide; UI flags it.
    #[serde(default)]
    pub missing: bool,
}

/// Scan only the Scanned Root for Course Folders. Used internally by
/// `library_view`; exposed for tests and for callers that don't yet have a
/// config (e.g. first-launch before pinning is possible).
pub fn scan_library(root: &Path) -> Result<Vec<CourseEntry>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    visit(root, &mut out);
    Ok(out)
}

/// Build the full Library view from the user's config: Scanned-Root entries
/// plus Pinned Folders, minus any Folders the user has explicitly forgotten.
/// Per ADR-0001:
/// - A Pinned Folder whose `course.json` is gone is flagged `missing` rather
///   than auto-removed — the user decides.
/// - A folder that is both inside the Scanned Root and pinned shows up once,
///   as a Scanned-Root entry (the scan wins; pinning is a no-op for it).
/// - Ignored Folders are filtered out — the bytes stay, the entry doesn't.
pub fn library_view(config: &AppConfig) -> Result<Vec<CourseEntry>> {
    let ignored: std::collections::HashSet<PathBuf> = config
        .ignored_folders
        .iter()
        .map(|p| canonical_or_raw(p))
        .collect();

    let mut out: Vec<CourseEntry> = Vec::new();
    if let Some(root) = config.scanned_root.as_deref() {
        out = scan_library(root)?
            .into_iter()
            .filter(|e| !ignored.contains(&canonical_or_raw(&e.folder)))
            .collect();
    }
    let already: std::collections::HashSet<PathBuf> =
        out.iter().map(|e| canonical_or_raw(&e.folder)).collect();

    for pinned in &config.pinned_folders {
        let key = canonical_or_raw(pinned);
        if already.contains(&key) || ignored.contains(&key) {
            continue;
        }
        out.push(pinned_entry(pinned));
    }
    Ok(out)
}

/// Add a Scanned-Root entry's folder to the ignored list so it stops
/// appearing in the Library. Idempotent.
pub fn ignore_folder(config: &mut AppConfig, folder: &Path) {
    let canonical = canonical_or_raw(folder);
    if config
        .ignored_folders
        .iter()
        .any(|p| canonical_or_raw(p) == canonical)
    {
        return;
    }
    config.ignored_folders.push(canonical);
}

/// Reverse of `ignore_folder`. Returns `true` if the entry was present.
pub fn unignore_folder(config: &mut AppConfig, folder: &Path) -> bool {
    let canonical = canonical_or_raw(folder);
    let before = config.ignored_folders.len();
    config
        .ignored_folders
        .retain(|p| canonical_or_raw(p) != canonical);
    before != config.ignored_folders.len()
}

fn pinned_entry(folder: &Path) -> CourseEntry {
    match entry_for(folder) {
        Some(mut e) => {
            e.source = CourseSource::Pinned;
            e
        }
        None => CourseEntry {
            folder: folder.to_path_buf(),
            title: folder
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("(missing)")
                .to_string(),
            modified_ms: 0,
            video_count: 0,
            source: CourseSource::Pinned,
            missing: true,
        },
    }
}

fn visit(dir: &Path, out: &mut Vec<CourseEntry>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.join(COURSE_JSON).is_file() {
            if let Some(course_entry) = entry_for(&path) {
                out.push(course_entry);
            }
            // Stop descent: course.json acts as a package sentinel (ADR-0001).
        } else {
            visit(&path, out);
        }
    }
}

/// Add a Course Folder living outside the Scanned Root to the Library.
/// Validates the folder is a Course Folder (has `course.json`); idempotent
/// when the same folder is pinned twice (canonical path comparison).
pub fn pin_folder(config: &mut AppConfig, folder: &Path) -> Result<()> {
    if !folder.join(COURSE_JSON).is_file() {
        return Err(CoreError::NotACourseFolder(folder.to_path_buf()));
    }
    let canonical = canonical_or_raw(folder);
    if config
        .pinned_folders
        .iter()
        .any(|p| canonical_or_raw(p) == canonical)
    {
        return Ok(());
    }
    config.pinned_folders.push(canonical);
    Ok(())
}

/// Remove a Pinned Folder from the Library. Does not touch the folder on disk.
/// Returns `true` if the entry was present.
pub fn unpin_folder(config: &mut AppConfig, folder: &Path) -> bool {
    let canonical = canonical_or_raw(folder);
    let before = config.pinned_folders.len();
    config
        .pinned_folders
        .retain(|p| canonical_or_raw(p) != canonical);
    before != config.pinned_folders.len()
}

fn canonical_or_raw(folder: &Path) -> PathBuf {
    std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf())
}

/// Send a Course Folder to the macOS Trash via the OS API (ADR-0001: there
/// is no hard-delete in the app). Refuses anything that isn't a Course Folder
/// to avoid the function being repurposed to trash arbitrary user paths.
pub fn move_course_folder_to_trash(folder: &Path) -> Result<()> {
    if !folder.join(COURSE_JSON).is_file() {
        return Err(CoreError::NotACourseFolder(folder.to_path_buf()));
    }
    trash::delete(folder).map_err(|e| CoreError::Trash {
        path: folder.to_path_buf(),
        message: e.to_string(),
    })
}

fn entry_for(folder: &Path) -> Option<CourseEntry> {
    let course = read_course(folder).ok()?;
    let modified_ms = std::fs::metadata(folder)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some(CourseEntry {
        folder: folder.to_path_buf(),
        title: course.title,
        modified_ms,
        video_count: course.videos.len(),
        source: CourseSource::Scanned,
        missing: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::course::create_course;

    #[test]
    fn scan_library_returns_empty_for_empty_root() {
        let root = tempfile::tempdir().unwrap();
        let entries = scan_library(root.path()).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn scan_library_returns_empty_when_root_does_not_exist() {
        let entries = scan_library(Path::new("/tmp/this/should/not/exist/courseforge-test-1234")).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn scan_library_finds_created_courses_with_title_and_zero_videos() {
        let root = tempfile::tempdir().unwrap();
        create_course(root.path(), "Alpha").unwrap();
        create_course(root.path(), "Beta").unwrap();

        let mut entries = scan_library(root.path()).unwrap();
        entries.sort_by(|a, b| a.title.cmp(&b.title));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Alpha");
        assert_eq!(entries[0].video_count, 0);
        assert_eq!(entries[1].title, "Beta");
        assert_eq!(entries[1].video_count, 0);
        assert!(entries[0].modified_ms > 0);
    }

    #[test]
    fn scan_library_ignores_dirs_without_course_json() {
        let root = tempfile::tempdir().unwrap();
        create_course(root.path(), "Real").unwrap();
        std::fs::create_dir(root.path().join("not-a-course")).unwrap();

        let entries = scan_library(root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Real");
    }

    #[test]
    fn scan_library_stops_descent_at_course_json_marker() {
        // Per ADR-0001: course.json is a package sentinel; we don't descend into
        // a Course Folder looking for nested courses.
        let root = tempfile::tempdir().unwrap();
        let outer = create_course(root.path(), "Outer").unwrap();
        let nested = outer.join("nested-course");
        std::fs::create_dir(&nested).unwrap();
        std::fs::write(
            nested.join("course.json"),
            r#"{"schemaVersion":1,"title":"Nested","modules":[],"videos":[]}"#,
        )
        .unwrap();

        let entries = scan_library(root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Outer");
    }

    #[test]
    fn scan_library_uses_json_title_not_folder_name_on_drift() {
        let root = tempfile::tempdir().unwrap();
        let original = create_course(root.path(), "Original Title").unwrap();
        std::fs::rename(&original, root.path().join("user-renamed-folder")).unwrap();

        let entries = scan_library(root.path()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Original Title");
        assert!(entries[0].folder.ends_with("user-renamed-folder"));
    }

    #[test]
    fn pin_folder_records_a_valid_course_folder() {
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = create_course(elsewhere.path(), "Pinned One").unwrap();
        let mut cfg = AppConfig::default();

        pin_folder(&mut cfg, &folder).unwrap();

        assert_eq!(cfg.pinned_folders.len(), 1);
        assert_eq!(
            canonical_or_raw(&cfg.pinned_folders[0]),
            canonical_or_raw(&folder)
        );
    }

    #[test]
    fn pin_folder_rejects_a_non_course_folder() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = AppConfig::default();
        let result = pin_folder(&mut cfg, dir.path());
        assert!(matches!(result, Err(CoreError::NotACourseFolder(_))));
        assert!(cfg.pinned_folders.is_empty());
    }

    #[test]
    fn pin_folder_is_idempotent_for_the_same_folder() {
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = create_course(elsewhere.path(), "P").unwrap();
        let mut cfg = AppConfig::default();

        pin_folder(&mut cfg, &folder).unwrap();
        pin_folder(&mut cfg, &folder).unwrap();

        assert_eq!(cfg.pinned_folders.len(), 1);
    }

    #[test]
    fn unpin_folder_removes_the_entry_and_returns_true() {
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = create_course(elsewhere.path(), "P").unwrap();
        let mut cfg = AppConfig::default();
        pin_folder(&mut cfg, &folder).unwrap();

        let removed = unpin_folder(&mut cfg, &folder);

        assert!(removed);
        assert!(cfg.pinned_folders.is_empty());
    }

    #[test]
    fn unpin_folder_returns_false_when_not_pinned() {
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = create_course(elsewhere.path(), "P").unwrap();
        let mut cfg = AppConfig::default();

        let removed = unpin_folder(&mut cfg, &folder);

        assert!(!removed);
    }

    #[test]
    fn unpin_folder_works_even_when_the_folder_is_gone() {
        // Real flow: the UI hands back the path we previously stored. If that
        // folder has since been deleted, unpinning must still succeed.
        let elsewhere = tempfile::tempdir().unwrap();
        let folder = create_course(elsewhere.path(), "P").unwrap();
        let mut cfg = AppConfig::default();
        pin_folder(&mut cfg, &folder).unwrap();
        let stored = cfg.pinned_folders[0].clone();
        std::fs::remove_dir_all(&folder).unwrap();

        let removed = unpin_folder(&mut cfg, &stored);
        assert!(removed);
        assert!(cfg.pinned_folders.is_empty());
    }

    #[test]
    fn move_course_folder_to_trash_removes_the_folder_from_disk() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Trashable").unwrap();
        assert!(folder.is_dir());

        move_course_folder_to_trash(&folder).unwrap();

        assert!(!folder.exists(), "folder should be gone from its original location");
    }

    #[test]
    fn move_course_folder_to_trash_refuses_a_non_course_folder() {
        // Defensive: this fn must never become a generic "trash any folder"
        // path — that's how data loss happens. ADR-0001 deletion is at Course
        // granularity.
        let dir = tempfile::tempdir().unwrap();
        let result = move_course_folder_to_trash(dir.path());
        assert!(matches!(result, Err(CoreError::NotACourseFolder(_))));
        assert!(dir.path().is_dir(), "non-course folder must not be touched");
    }

    #[test]
    fn library_view_combines_scanned_root_and_pinned_folders() {
        let scanned = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        create_course(scanned.path(), "Scanned One").unwrap();
        let pinned = create_course(elsewhere.path(), "Pinned One").unwrap();
        let cfg = AppConfig {
            scanned_root: Some(scanned.path().to_path_buf()),
            pinned_folders: vec![pinned],
            ignored_folders: Vec::new(),
        };

        let mut entries = library_view(&cfg).unwrap();
        entries.sort_by(|a, b| a.title.cmp(&b.title));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].title, "Pinned One");
        assert_eq!(entries[0].source, CourseSource::Pinned);
        assert!(!entries[0].missing);
        assert_eq!(entries[1].title, "Scanned One");
        assert_eq!(entries[1].source, CourseSource::Scanned);
    }

    #[test]
    fn library_view_works_without_a_scanned_root() {
        let elsewhere = tempfile::tempdir().unwrap();
        let pinned = create_course(elsewhere.path(), "Only Pinned").unwrap();
        let cfg = AppConfig {
            scanned_root: None,
            pinned_folders: vec![pinned],
            ignored_folders: Vec::new(),
        };

        let entries = library_view(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Only Pinned");
        assert_eq!(entries[0].source, CourseSource::Pinned);
    }

    #[test]
    fn library_view_flags_pinned_folder_that_is_gone() {
        // ADR-0001 / issue #4: a missing pinned entry is surfaced, not removed.
        let elsewhere = tempfile::tempdir().unwrap();
        let pinned = create_course(elsewhere.path(), "Vanished").unwrap();
        let mut cfg = AppConfig {
            scanned_root: None,
            pinned_folders: vec![pinned.clone()],
            ignored_folders: Vec::new(),
        };
        // Read the stored canonical path so the test mimics the real flow
        // (the path the UI would later send back).
        let stored = cfg.pinned_folders[0].clone();
        std::fs::remove_dir_all(&pinned).unwrap();

        let entries = library_view(&cfg).unwrap();

        assert_eq!(entries.len(), 1);
        assert!(entries[0].missing);
        assert_eq!(entries[0].source, CourseSource::Pinned);
        assert_eq!(entries[0].folder, stored);
        // And we have not auto-unpinned it.
        assert_eq!(cfg.pinned_folders.len(), 1);
        // No-op compile fence to keep cfg as `mut` (we mutated nothing on
        // purpose — verifying that explicitly).
        let _ = &mut cfg;
    }

    #[test]
    fn library_view_filters_out_ignored_scanned_root_entries() {
        let scanned = tempfile::tempdir().unwrap();
        let keep = create_course(scanned.path(), "Keep").unwrap();
        let forget = create_course(scanned.path(), "Forget").unwrap();
        let mut cfg = AppConfig {
            scanned_root: Some(scanned.path().to_path_buf()),
            pinned_folders: Vec::new(),
            ignored_folders: Vec::new(),
        };
        ignore_folder(&mut cfg, &forget);

        let entries = library_view(&cfg).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Keep");
        assert_eq!(canonical_or_raw(&entries[0].folder), canonical_or_raw(&keep));
        // The forgotten folder is still on disk.
        assert!(forget.join("course.json").is_file());
    }

    #[test]
    fn ignore_folder_is_idempotent() {
        let scanned = tempfile::tempdir().unwrap();
        let folder = create_course(scanned.path(), "F").unwrap();
        let mut cfg = AppConfig {
            scanned_root: Some(scanned.path().to_path_buf()),
            pinned_folders: Vec::new(),
            ignored_folders: Vec::new(),
        };
        ignore_folder(&mut cfg, &folder);
        ignore_folder(&mut cfg, &folder);
        assert_eq!(cfg.ignored_folders.len(), 1);
    }

    #[test]
    fn unignore_folder_re_surfaces_the_entry() {
        let scanned = tempfile::tempdir().unwrap();
        let folder = create_course(scanned.path(), "Returns").unwrap();
        let mut cfg = AppConfig {
            scanned_root: Some(scanned.path().to_path_buf()),
            pinned_folders: Vec::new(),
            ignored_folders: Vec::new(),
        };
        ignore_folder(&mut cfg, &folder);
        assert!(library_view(&cfg).unwrap().is_empty());

        let removed = unignore_folder(&mut cfg, &folder);
        assert!(removed);
        let entries = library_view(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "Returns");
    }

    #[test]
    fn library_view_does_not_double_count_a_pinned_folder_inside_the_scanned_root() {
        let scanned = tempfile::tempdir().unwrap();
        let inside = create_course(scanned.path(), "Inside").unwrap();
        let cfg = AppConfig {
            scanned_root: Some(scanned.path().to_path_buf()),
            pinned_folders: vec![inside],
            ignored_folders: Vec::new(),
        };

        let entries = library_view(&cfg).unwrap();
        assert_eq!(entries.len(), 1);
        // The Scanned-Root entry wins — pinning was redundant.
        assert_eq!(entries[0].source, CourseSource::Scanned);
    }

    #[test]
    fn scan_library_groups_videos_by_course() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Has Videos").unwrap();
        std::fs::write(
            folder.join("course.json"),
            r#"{"schemaVersion":1,"title":"Has Videos","modules":[],"videos":[{"id":"v1","title":"a"},{"id":"v2","title":"b"},{"id":"v3","title":"c"}]}"#,
        )
        .unwrap();

        let entries = scan_library(root.path()).unwrap();
        assert_eq!(entries[0].video_count, 3);
    }
}
