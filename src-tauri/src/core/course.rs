use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};
use crate::core::slug;

pub const COURSE_JSON: &str = "course.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Course {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub title: String,
    pub modules: Vec<serde_json::Value>,
    pub videos: Vec<serde_json::Value>,
}

impl Course {
    pub fn new(title: String) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            title,
            modules: Vec::new(),
            videos: Vec::new(),
        }
    }
}

pub fn create_course(root: &Path, title: &str) -> Result<PathBuf> {
    if title.trim().is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    std::fs::create_dir_all(root).map_err(|e| CoreError::Io {
        path: root.to_path_buf(),
        source: e,
    })?;
    let slug = slug::unique_slug_in(root, title);
    let folder = root.join(slug);
    std::fs::create_dir(&folder).map_err(|e| CoreError::Io {
        path: folder.clone(),
        source: e,
    })?;
    let course = Course::new(title.to_string());
    write_course(&folder, &course)?;
    Ok(folder)
}

fn write_course(folder: &Path, course: &Course) -> Result<()> {
    let path = folder.join(COURSE_JSON);
    let json = serde_json::to_string_pretty(course).map_err(|e| CoreError::InvalidCourseJson {
        path: path.clone(),
        source: e,
    })?;
    std::fs::write(&path, json).map_err(|e| CoreError::Io { path, source: e })
}

pub fn read_course(folder: &Path) -> Result<Course> {
    let path = folder.join(COURSE_JSON);
    if !path.is_file() {
        return Err(CoreError::NotACourseFolder(folder.to_path_buf()));
    }
    let bytes = std::fs::read(&path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| CoreError::InvalidCourseJson { path, source: e })
}

pub fn rename_course(folder: &Path, new_title: &str) -> Result<PathBuf> {
    if new_title.trim().is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    let mut course = read_course(folder)?;
    course.title = new_title.to_string();
    write_course(folder, &course)?;

    // Best-effort folder rename. JSON title always wins, so a collision or
    // failure is non-fatal — we keep the existing folder name.
    let parent = match folder.parent() {
        Some(p) => p,
        None => return Ok(folder.to_path_buf()),
    };
    let desired_slug = slug::slugify(new_title);
    let target = parent.join(&desired_slug);
    if target == folder || target.exists() {
        return Ok(folder.to_path_buf());
    }
    match std::fs::rename(folder, &target) {
        Ok(()) => Ok(target),
        Err(_) => Ok(folder.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_course_resolves_collisions_deterministically() {
        let root = tempfile::tempdir().unwrap();
        let first = create_course(root.path(), "My Course").unwrap();
        let second = create_course(root.path(), "My Course").unwrap();

        assert_eq!(first, root.path().join("my-course"));
        assert_eq!(second, root.path().join("my-course-2"));
        assert!(first.join(COURSE_JSON).is_file());
        assert!(second.join(COURSE_JSON).is_file());
    }

    #[test]
    fn rename_course_updates_json_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Old Title").unwrap();
        let new_folder = rename_course(&folder, "New Title").unwrap();

        let course = read_course(&new_folder).unwrap();
        assert_eq!(course.title, "New Title");
    }

    #[test]
    fn rename_course_renames_folder_to_new_slug_when_no_collision() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Old Title").unwrap();
        let new_folder = rename_course(&folder, "Shiny New").unwrap();

        assert_eq!(new_folder, root.path().join("shiny-new"));
        assert!(!folder.exists());
        assert!(new_folder.join(COURSE_JSON).is_file());
    }

    #[test]
    fn rename_course_keeps_folder_name_when_target_slug_collides() {
        // Title drift is permitted (ADR-0001). JSON title wins; folder rename
        // is best-effort and yields to existing folders.
        let root = tempfile::tempdir().unwrap();
        let a = create_course(root.path(), "Alpha").unwrap();
        let _b = create_course(root.path(), "Beta").unwrap();

        let result = rename_course(&a, "Beta").unwrap();
        assert_eq!(result, a, "should not have moved");
        assert!(a.exists());
        assert_eq!(read_course(&a).unwrap().title, "Beta");
    }

    #[test]
    fn rename_course_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "X").unwrap();
        let result = rename_course(&folder, "  ");
        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn read_course_round_trips_what_create_course_wrote() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Roundtrip").unwrap();

        let course = read_course(&folder).unwrap();
        assert_eq!(course.schema_version, SCHEMA_VERSION);
        assert_eq!(course.title, "Roundtrip");
        assert!(course.modules.is_empty());
        assert!(course.videos.is_empty());
    }

    #[test]
    fn read_course_errors_when_marker_missing() {
        let root = tempfile::tempdir().unwrap();
        let result = read_course(root.path());
        assert!(matches!(result, Err(CoreError::NotACourseFolder(_))));
    }

    #[test]
    fn create_course_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let result = create_course(root.path(), "   ");
        assert!(matches!(result, Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn create_course_writes_valid_course_json() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "Hello World").unwrap();

        assert_eq!(folder, root.path().join("hello-world"));
        assert!(folder.is_dir());

        let course_json = folder.join(COURSE_JSON);
        assert!(course_json.is_file());

        let parsed: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&course_json).unwrap()).unwrap();
        assert_eq!(parsed["schemaVersion"], 1);
        assert_eq!(parsed["title"], "Hello World");
        assert_eq!(parsed["modules"], serde_json::json!([]));
        assert_eq!(parsed["videos"], serde_json::json!([]));
    }
}
