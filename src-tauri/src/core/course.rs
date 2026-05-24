use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};
use crate::core::slug;

pub const COURSE_JSON: &str = "course.json";
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Module {
    pub id: String,
    pub title: String,
    #[serde(rename = "videoIds", default)]
    pub video_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Video {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Course {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub title: String,
    pub modules: Vec<Module>,
    pub videos: Vec<Video>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
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
    write_atomic(&path, json.as_bytes())
}

/// Atomic write: serialise to a sibling `.tmp` file, fsync, then rename
/// over the destination. A crash mid-write leaves either the previous file
/// intact or the new one fully written — never a half-written `course.json`.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::File::create(&tmp).map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.write_all(bytes).map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
        f.sync_all().map_err(|e| CoreError::Io {
            path: tmp.clone(),
            source: e,
        })?;
    }
    std::fs::rename(&tmp, path).map_err(|e| CoreError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

fn mutate_course<F>(folder: &Path, f: F) -> Result<Course>
where
    F: FnOnce(&mut Course) -> Result<()>,
{
    let mut course = read_course(folder)?;
    f(&mut course)?;
    write_course(folder, &course)?;
    Ok(course)
}

pub fn add_module(folder: &Path, title: &str) -> Result<Module> {
    let title = title.trim();
    if title.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    let module = Module {
        id: new_id(),
        title: title.to_string(),
        video_ids: Vec::new(),
    };
    let added = module.clone();
    mutate_course(folder, |c| {
        c.modules.push(module);
        Ok(())
    })?;
    Ok(added)
}

pub fn rename_video(folder: &Path, video_id: &str, new_title: &str) -> Result<()> {
    let title = new_title.trim();
    if title.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    mutate_course(folder, |c| {
        let v = c.videos.iter_mut()
            .find(|v| v.id == video_id)
            .ok_or_else(|| CoreError::VideoNotFound(video_id.to_string()))?;
        v.title = title.to_string();
        Ok(())
    })?;
    Ok(())
}

pub fn reorder_videos_in_module(folder: &Path, module_id: &str, ordered_ids: &[String]) -> Result<()> {
    mutate_course(folder, |c| {
        let m = c.modules.iter_mut()
            .find(|m| m.id == module_id)
            .ok_or_else(|| CoreError::ModuleNotFound(module_id.to_string()))?;
        if !same_set(ordered_ids, m.video_ids.iter().map(String::as_str)) {
            return Err(CoreError::ReorderMismatch);
        }
        m.video_ids = ordered_ids.to_vec();
        Ok(())
    })?;
    Ok(())
}

pub fn delete_video(folder: &Path, video_id: &str) -> Result<()> {
    mutate_course(folder, |c| {
        let idx = c.videos.iter()
            .position(|v| v.id == video_id)
            .ok_or_else(|| CoreError::VideoNotFound(video_id.to_string()))?;
        c.videos.remove(idx);
        for m in &mut c.modules {
            m.video_ids.retain(|id| id != video_id);
        }
        Ok(())
    })?;
    Ok(())
}

/// Move a Video to (a possibly different) Module at the given insertion index.
/// This is a `course.json` array edit only — the on-disk `videos/<video-id>/`
/// folder, if any, is untouched (ADR-0001).
pub fn move_video_to_module(
    folder: &Path,
    video_id: &str,
    target_module_id: &str,
    index: usize,
) -> Result<()> {
    mutate_course(folder, |c| {
        if !c.videos.iter().any(|v| v.id == video_id) {
            return Err(CoreError::VideoNotFound(video_id.to_string()));
        }
        if !c.modules.iter().any(|m| m.id == target_module_id) {
            return Err(CoreError::ModuleNotFound(target_module_id.to_string()));
        }
        for m in &mut c.modules {
            m.video_ids.retain(|id| id != video_id);
        }
        let target = c.modules.iter_mut().find(|m| m.id == target_module_id).unwrap();
        let insert_at = index.min(target.video_ids.len());
        target.video_ids.insert(insert_at, video_id.to_string());
        Ok(())
    })?;
    Ok(())
}

pub fn delete_module(folder: &Path, module_id: &str) -> Result<()> {
    mutate_course(folder, |c| {
        let idx = c.modules.iter()
            .position(|m| m.id == module_id)
            .ok_or_else(|| CoreError::ModuleNotFound(module_id.to_string()))?;
        let removed = c.modules.remove(idx);
        let doomed: std::collections::HashSet<&str> =
            removed.video_ids.iter().map(String::as_str).collect();
        c.videos.retain(|v| !doomed.contains(v.id.as_str()));
        Ok(())
    })?;
    Ok(())
}

pub fn add_video(folder: &Path, module_id: &str, title: &str) -> Result<Video> {
    let title = title.trim();
    if title.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    let video = Video { id: new_id(), title: title.to_string() };
    let added = video.clone();
    mutate_course(folder, |c| {
        let m = c.modules.iter_mut()
            .find(|m| m.id == module_id)
            .ok_or_else(|| CoreError::ModuleNotFound(module_id.to_string()))?;
        m.video_ids.push(video.id.clone());
        c.videos.push(video);
        Ok(())
    })?;
    Ok(added)
}

pub fn reorder_modules(folder: &Path, ordered_ids: &[String]) -> Result<()> {
    mutate_course(folder, |c| {
        if !same_set(ordered_ids, c.modules.iter().map(|m| m.id.as_str())) {
            return Err(CoreError::ReorderMismatch);
        }
        c.modules.sort_by_key(|m| {
            ordered_ids.iter().position(|id| id == &m.id).unwrap()
        });
        Ok(())
    })?;
    Ok(())
}

fn same_set<'a, I>(ordered: &[String], existing: I) -> bool
where
    I: Iterator<Item = &'a str>,
{
    let existing: std::collections::HashSet<&str> = existing.collect();
    if ordered.len() != existing.len() {
        return false;
    }
    let proposed: std::collections::HashSet<&str> = ordered.iter().map(String::as_str).collect();
    proposed == existing
}

pub fn rename_module(folder: &Path, module_id: &str, new_title: &str) -> Result<()> {
    let title = new_title.trim();
    if title.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    mutate_course(folder, |c| {
        let m = c.modules.iter_mut()
            .find(|m| m.id == module_id)
            .ok_or_else(|| CoreError::ModuleNotFound(module_id.to_string()))?;
        m.title = title.to_string();
        Ok(())
    })?;
    Ok(())
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

    #[test]
    fn add_module_appends_a_module_with_fresh_id_and_persists() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();

        let m = add_module(&folder, "Intro").unwrap();
        assert!(!m.id.is_empty());
        assert_eq!(m.title, "Intro");
        assert!(m.video_ids.is_empty());

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules.len(), 1);
        assert_eq!(on_disk.modules[0], m);
    }

    #[test]
    fn add_module_assigns_distinct_ids_to_successive_modules() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let a = add_module(&folder, "A").unwrap();
        let b = add_module(&folder, "B").unwrap();
        assert_ne!(a.id, b.id);

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules.iter().map(|m| &m.title).collect::<Vec<_>>(),
                   vec!["A", "B"]);
    }

    #[test]
    fn add_module_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        assert!(matches!(add_module(&folder, "  "), Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn rename_module_updates_title_in_place() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "Old").unwrap();

        rename_module(&folder, &m.id, "New").unwrap();

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules[0].id, m.id);
        assert_eq!(on_disk.modules[0].title, "New");
    }

    #[test]
    fn rename_module_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "Old").unwrap();
        assert!(matches!(rename_module(&folder, &m.id, " "), Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn rename_module_errors_when_module_not_found() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let result = rename_module(&folder, "no-such-id", "X");
        assert!(matches!(result, Err(CoreError::ModuleNotFound(_))));
    }

    #[test]
    fn reorder_modules_permutes_the_module_list() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let a = add_module(&folder, "A").unwrap();
        let b = add_module(&folder, "B").unwrap();
        let c = add_module(&folder, "C").unwrap();

        reorder_modules(&folder, &[c.id.clone(), a.id.clone(), b.id.clone()]).unwrap();

        let on_disk = read_course(&folder).unwrap();
        let titles: Vec<_> = on_disk.modules.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titles, vec!["C", "A", "B"]);
    }

    #[test]
    fn reorder_modules_rejects_mismatched_id_set() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let a = add_module(&folder, "A").unwrap();
        add_module(&folder, "B").unwrap();

        // Missing one of the existing ids — should reject and not mutate.
        let result = reorder_modules(&folder, &[a.id.clone()]);
        assert!(matches!(result, Err(CoreError::ReorderMismatch)));
        assert_eq!(read_course(&folder).unwrap().modules.len(), 2);
    }

    #[test]
    fn add_video_appends_video_and_registers_it_with_module() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();

        let v = add_video(&folder, &m.id, "Intro slot").unwrap();
        assert!(!v.id.is_empty());
        assert_eq!(v.title, "Intro slot");

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.videos, vec![v.clone()]);
        assert_eq!(on_disk.modules[0].video_ids, vec![v.id]);
    }

    #[test]
    fn add_video_appends_to_target_module_order() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let a = add_video(&folder, &m.id, "A").unwrap();
        let b = add_video(&folder, &m.id, "B").unwrap();

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules[0].video_ids, vec![a.id, b.id]);
    }

    #[test]
    fn add_video_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        assert!(matches!(add_video(&folder, &m.id, "  "), Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn add_video_errors_when_module_not_found() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let result = add_video(&folder, "no-such-id", "A");
        assert!(matches!(result, Err(CoreError::ModuleNotFound(_))));
    }

    #[test]
    fn delete_module_removes_it_and_cascades_to_its_videos() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let keep = add_module(&folder, "Keep").unwrap();
        let drop = add_module(&folder, "Drop").unwrap();
        let v_kept = add_video(&folder, &keep.id, "K").unwrap();
        let v_gone = add_video(&folder, &drop.id, "G").unwrap();

        delete_module(&folder, &drop.id).unwrap();

        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules.iter().map(|m| &m.id).collect::<Vec<_>>(), vec![&keep.id]);
        let video_ids: Vec<_> = on_disk.videos.iter().map(|v| v.id.clone()).collect();
        assert_eq!(video_ids, vec![v_kept.id]);
        assert!(!video_ids.contains(&v_gone.id));
    }

    #[test]
    fn delete_module_errors_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        assert!(matches!(delete_module(&folder, "no-such-id"), Err(CoreError::ModuleNotFound(_))));
    }

    #[test]
    fn rename_video_updates_title_in_place() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let v = add_video(&folder, &m.id, "Old").unwrap();

        rename_video(&folder, &v.id, "New").unwrap();
        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.videos[0].id, v.id);
        assert_eq!(on_disk.videos[0].title, "New");
    }

    #[test]
    fn rename_video_rejects_empty_title() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let v = add_video(&folder, &m.id, "Old").unwrap();
        assert!(matches!(rename_video(&folder, &v.id, " "), Err(CoreError::EmptyTitle)));
    }

    #[test]
    fn rename_video_errors_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        assert!(matches!(rename_video(&folder, "no-such-id", "X"), Err(CoreError::VideoNotFound(_))));
    }

    #[test]
    fn reorder_videos_in_module_permutes_only_that_modules_order() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m1 = add_module(&folder, "M1").unwrap();
        let m2 = add_module(&folder, "M2").unwrap();
        let a = add_video(&folder, &m1.id, "A").unwrap();
        let b = add_video(&folder, &m1.id, "B").unwrap();
        let c = add_video(&folder, &m1.id, "C").unwrap();
        let z = add_video(&folder, &m2.id, "Z").unwrap();

        reorder_videos_in_module(&folder, &m1.id, &[c.id.clone(), a.id.clone(), b.id.clone()])
            .unwrap();

        let on_disk = read_course(&folder).unwrap();
        let m1_after = on_disk.modules.iter().find(|m| m.id == m1.id).unwrap();
        let m2_after = on_disk.modules.iter().find(|m| m.id == m2.id).unwrap();
        assert_eq!(m1_after.video_ids, vec![c.id, a.id, b.id]);
        assert_eq!(m2_after.video_ids, vec![z.id]);
    }

    #[test]
    fn reorder_videos_in_module_rejects_mismatched_id_set() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let a = add_video(&folder, &m.id, "A").unwrap();
        add_video(&folder, &m.id, "B").unwrap();

        let result = reorder_videos_in_module(&folder, &m.id, &[a.id]);
        assert!(matches!(result, Err(CoreError::ReorderMismatch)));
    }

    #[test]
    fn delete_video_removes_from_videos_and_its_module() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let a = add_video(&folder, &m.id, "A").unwrap();
        let b = add_video(&folder, &m.id, "B").unwrap();

        delete_video(&folder, &a.id).unwrap();
        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.videos.iter().map(|v| v.id.clone()).collect::<Vec<_>>(), vec![b.id.clone()]);
        assert_eq!(on_disk.modules[0].video_ids, vec![b.id]);
    }

    #[test]
    fn delete_video_errors_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        assert!(matches!(delete_video(&folder, "no-such-id"), Err(CoreError::VideoNotFound(_))));
    }

    #[test]
    fn move_video_to_module_relocates_id_without_touching_videos_array_order() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let src = add_module(&folder, "Src").unwrap();
        let dst = add_module(&folder, "Dst").unwrap();
        let a = add_video(&folder, &src.id, "A").unwrap();
        let b = add_video(&folder, &src.id, "B").unwrap();
        let x = add_video(&folder, &dst.id, "X").unwrap();

        move_video_to_module(&folder, &a.id, &dst.id, 1).unwrap();

        let on_disk = read_course(&folder).unwrap();
        let src_after = on_disk.modules.iter().find(|m| m.id == src.id).unwrap();
        let dst_after = on_disk.modules.iter().find(|m| m.id == dst.id).unwrap();
        assert_eq!(src_after.video_ids, vec![b.id]);
        assert_eq!(dst_after.video_ids, vec![x.id, a.id]);

        // The top-level videos[] array stays as-is; Video {a} still exists with its id.
        let video_ids: std::collections::HashSet<_> =
            on_disk.videos.iter().map(|v| v.id.clone()).collect();
        assert_eq!(video_ids.len(), 3);
    }

    #[test]
    fn move_video_to_module_clamps_index_to_end_of_target() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let src = add_module(&folder, "Src").unwrap();
        let dst = add_module(&folder, "Dst").unwrap();
        let a = add_video(&folder, &src.id, "A").unwrap();
        let x = add_video(&folder, &dst.id, "X").unwrap();

        // index way past the end -> append.
        move_video_to_module(&folder, &a.id, &dst.id, 999).unwrap();
        let on_disk = read_course(&folder).unwrap();
        let dst_after = on_disk.modules.iter().find(|m| m.id == dst.id).unwrap();
        assert_eq!(dst_after.video_ids, vec![x.id, a.id]);
    }

    #[test]
    fn move_video_to_module_within_same_module_reorders() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let a = add_video(&folder, &m.id, "A").unwrap();
        let b = add_video(&folder, &m.id, "B").unwrap();
        let c = add_video(&folder, &m.id, "C").unwrap();

        // move A to the end.
        move_video_to_module(&folder, &a.id, &m.id, 2).unwrap();
        let on_disk = read_course(&folder).unwrap();
        assert_eq!(on_disk.modules[0].video_ids, vec![b.id, c.id, a.id]);
    }

    #[test]
    fn move_video_to_module_errors_for_missing_video_or_module() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        let m = add_module(&folder, "M").unwrap();
        let v = add_video(&folder, &m.id, "V").unwrap();

        assert!(matches!(
            move_video_to_module(&folder, "nope", &m.id, 0),
            Err(CoreError::VideoNotFound(_))
        ));
        assert!(matches!(
            move_video_to_module(&folder, &v.id, "nope", 0),
            Err(CoreError::ModuleNotFound(_))
        ));
    }

    #[test]
    fn write_course_leaves_no_temp_file_behind() {
        let root = tempfile::tempdir().unwrap();
        let folder = create_course(root.path(), "C").unwrap();
        add_module(&folder, "A").unwrap();

        let entries: Vec<_> = std::fs::read_dir(&folder).unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert!(entries.iter().any(|n| n == COURSE_JSON));
        assert!(!entries.iter().any(|n| n.to_string_lossy().ends_with(".tmp")));
    }
}
