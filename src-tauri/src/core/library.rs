use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;
use serde::{Deserialize, Serialize};
use crate::core::course::{read_course, COURSE_JSON};
use crate::core::error::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseEntry {
    pub folder: PathBuf,
    pub title: String,
    /// Last-modified time of the Course Folder, as Unix milliseconds.
    pub modified_ms: i64,
    pub video_count: usize,
}

pub fn scan_library(root: &Path) -> Result<Vec<CourseEntry>> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Ok(out);
    }
    visit(root, &mut out);
    Ok(out)
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
    fn scan_library_groups_videos_by_course() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Has Videos").unwrap();
        std::fs::write(
            folder.join("course.json"),
            r#"{"schemaVersion":1,"title":"Has Videos","modules":[],"videos":[{"id":"v1"},{"id":"v2"},{"id":"v3"}]}"#,
        )
        .unwrap();

        let entries = scan_library(root.path()).unwrap();
        assert_eq!(entries[0].video_count, 3);
    }
}
