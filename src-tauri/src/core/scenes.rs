//! Scenes — named, reusable Capture presets at the Course level.
//!
//! Per ADR-0002 a Scene binds an ordered set of [`SourceRole`]s to specific
//! devices plus per-source composition defaults. Hitting Record uses a
//! Scene to build the `Vec<CaptureRequest>` the recorder backend already
//! accepts. Scenes persist alongside `course.json` in a sibling
//! `scenes.json` at the Course Folder root.
//!
//! Phase 2 slice 1 (issue #33) introduces the model + on-disk format + the
//! minimum-viable CRUD surface. Rows hold a placeholder `Device { id:
//! "default", … }` until slice 2 (issue #34) adds the device picker; the
//! canvas preview lands in slice 7 (issue #39). No transcript-source
//! designation yet — that comes in slice 8 (issue #40).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::capture::{CompositionDefaults, Device, SourceRole};
use crate::core::error::{CoreError, Result};

pub const SCENES_JSON: &str = "scenes.json";
pub const SCHEMA_VERSION: u32 = 1;

/// One source row inside a [`Scene`]. Mirrors the runtime
/// [`crate::core::capture::CaptureRequest`] shape so building a Take from a
/// Scene is a straight field copy at Record time. The placeholder
/// `Device { id: "default", label: "Default" }` is the slice-1 sentinel
/// for "use the system default" until slice 2 adds the device picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SceneSource {
    pub role: SourceRole,
    pub device: Device,
    #[serde(default)]
    pub defaults: CompositionDefaults,
}

impl SceneSource {
    /// Build a SceneSource with the slice-1 placeholder device for the
    /// given role. Device picker (issue #34) replaces this with a real
    /// device id.
    pub fn placeholder(role: SourceRole) -> Self {
        Self {
            role,
            device: Device {
                id: "default".into(),
                label: "Default".into(),
            },
            defaults: CompositionDefaults::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scene {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub sources: Vec<SceneSource>,
}

/// On-disk shape of `scenes.json` — the wrapper carries the
/// `schemaVersion` so future shape bumps stay backwards-compatible.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenesFile {
    pub schema_version: u32,
    #[serde(default)]
    pub scenes: Vec<Scene>,
}

impl Default for ScenesFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            scenes: Vec::new(),
        }
    }
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn scenes_path(folder: &Path) -> PathBuf {
    folder.join(SCENES_JSON)
}

/// Read `scenes.json` if it exists. Missing file = empty Scene list,
/// per the issue's "Missing file = empty Scene list (acceptable; not an
/// error)" acceptance criterion.
pub fn read_scenes(folder: &Path) -> Result<ScenesFile> {
    let path = scenes_path(folder);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<ScenesFile>(&bytes)
            .map_err(|e| CoreError::InvalidScenesJson { path, source: e }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ScenesFile::default()),
        Err(e) => Err(CoreError::Io { path, source: e }),
    }
}

fn write_scenes(folder: &Path, file: &ScenesFile) -> Result<()> {
    let path = scenes_path(folder);
    let json = serde_json::to_string_pretty(file)
        .map_err(|e| CoreError::InvalidScenesJson { path: path.clone(), source: e })?;
    write_atomic(&path, json.as_bytes())
}

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

fn mutate_scenes<F, T>(folder: &Path, f: F) -> Result<T>
where
    F: FnOnce(&mut ScenesFile) -> Result<T>,
{
    let mut file = read_scenes(folder)?;
    let out = f(&mut file)?;
    write_scenes(folder, &file)?;
    Ok(out)
}

pub fn list_scenes(folder: &Path) -> Result<Vec<Scene>> {
    Ok(read_scenes(folder)?.scenes)
}

pub fn create_scene(folder: &Path, name: &str) -> Result<Scene> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    let scene = Scene {
        id: new_id(),
        name: name.to_string(),
        sources: Vec::new(),
    };
    let out = scene.clone();
    mutate_scenes(folder, |f| {
        f.scenes.push(scene);
        Ok(())
    })?;
    Ok(out)
}

pub fn rename_scene(folder: &Path, scene_id: &str, new_name: &str) -> Result<()> {
    let name = new_name.trim();
    if name.is_empty() {
        return Err(CoreError::EmptyTitle);
    }
    mutate_scenes(folder, |f| {
        let s = find_scene_mut(f, scene_id)?;
        s.name = name.to_string();
        Ok(())
    })
}

pub fn duplicate_scene(folder: &Path, scene_id: &str) -> Result<Scene> {
    mutate_scenes(folder, |f| {
        let pos = f
            .scenes
            .iter()
            .position(|s| s.id == scene_id)
            .ok_or_else(|| CoreError::SceneNotFound(scene_id.to_string()))?;
        let src = &f.scenes[pos];
        let copy = Scene {
            id: new_id(),
            name: format!("{} Copy", src.name),
            sources: src.sources.clone(),
        };
        let out = copy.clone();
        f.scenes.insert(pos + 1, copy);
        Ok(out)
    })
}

pub fn delete_scene(folder: &Path, scene_id: &str) -> Result<()> {
    mutate_scenes(folder, |f| {
        let pos = f
            .scenes
            .iter()
            .position(|s| s.id == scene_id)
            .ok_or_else(|| CoreError::SceneNotFound(scene_id.to_string()))?;
        f.scenes.remove(pos);
        Ok(())
    })
}

pub fn add_scene_source(folder: &Path, scene_id: &str, role: SourceRole) -> Result<SceneSource> {
    let source = SceneSource::placeholder(role);
    let out = source.clone();
    mutate_scenes(folder, |f| {
        let s = find_scene_mut(f, scene_id)?;
        s.sources.push(source);
        Ok(())
    })?;
    Ok(out)
}

pub fn remove_scene_source(folder: &Path, scene_id: &str, source_index: usize) -> Result<()> {
    mutate_scenes(folder, |f| {
        let s = find_scene_mut(f, scene_id)?;
        if source_index >= s.sources.len() {
            return Err(CoreError::SceneSourceIndexOutOfBounds {
                scene_id: scene_id.to_string(),
                index: source_index,
                len: s.sources.len(),
            });
        }
        s.sources.remove(source_index);
        Ok(())
    })
}

fn find_scene_mut<'a>(file: &'a mut ScenesFile, scene_id: &str) -> Result<&'a mut Scene> {
    file.scenes
        .iter_mut()
        .find(|s| s.id == scene_id)
        .ok_or_else(|| CoreError::SceneNotFound(scene_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp() -> TempDir {
        TempDir::new().unwrap()
    }

    #[test]
    fn scene_round_trips_as_camel_case_json() {
        let scene = Scene {
            id: "scene-1".into(),
            name: "Screencast".into(),
            sources: vec![SceneSource::placeholder(SourceRole::Screen)],
        };
        let json = serde_json::to_string(&scene).unwrap();
        assert!(json.contains("\"id\":\"scene-1\""));
        assert!(json.contains("\"name\":\"Screencast\""));
        assert!(json.contains("\"sources\""));
        assert!(!json.contains("source_role"));
        let back: Scene = serde_json::from_str(&json).unwrap();
        assert_eq!(back, scene);
    }

    #[test]
    fn scenes_file_round_trips_with_schema_version() {
        let file = ScenesFile {
            schema_version: 1,
            scenes: vec![Scene {
                id: "scene-1".into(),
                name: "A".into(),
                sources: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&file).unwrap();
        assert!(json.contains("\"schemaVersion\":1"));
        let back: ScenesFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, file);
    }

    #[test]
    fn read_scenes_returns_empty_when_file_is_missing() {
        let dir = tmp();
        let f = read_scenes(dir.path()).unwrap();
        assert_eq!(f.schema_version, SCHEMA_VERSION);
        assert!(f.scenes.is_empty());
    }

    #[test]
    fn read_scenes_after_create_returns_what_was_written() {
        let dir = tmp();
        let scene = create_scene(dir.path(), "Talking Head").unwrap();
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, scene.id);
        assert_eq!(list[0].name, "Talking Head");
    }

    #[test]
    fn create_scene_rejects_empty_name() {
        let dir = tmp();
        let err = create_scene(dir.path(), "   ").unwrap_err();
        assert!(matches!(err, CoreError::EmptyTitle));
        assert!(list_scenes(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn create_scene_trims_name() {
        let dir = tmp();
        let s = create_scene(dir.path(), "  Demo  ").unwrap();
        assert_eq!(s.name, "Demo");
    }

    #[test]
    fn rename_scene_changes_name_on_disk() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        rename_scene(dir.path(), &s.id, "B").unwrap();
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list[0].name, "B");
    }

    #[test]
    fn rename_scene_rejects_empty_name() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        let err = rename_scene(dir.path(), &s.id, "").unwrap_err();
        assert!(matches!(err, CoreError::EmptyTitle));
    }

    #[test]
    fn rename_unknown_scene_is_an_error() {
        let dir = tmp();
        let err = rename_scene(dir.path(), "nope", "X").unwrap_err();
        assert!(matches!(err, CoreError::SceneNotFound(_)));
    }

    #[test]
    fn duplicate_scene_inserts_a_copy_right_after_the_original() {
        let dir = tmp();
        let a = create_scene(dir.path(), "A").unwrap();
        let _b = create_scene(dir.path(), "B").unwrap();
        let dup = duplicate_scene(dir.path(), &a.id).unwrap();
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].id, a.id);
        assert_eq!(list[1].id, dup.id);
        assert_eq!(list[1].name, "A Copy");
        assert_ne!(dup.id, a.id);
    }

    #[test]
    fn duplicate_scene_copies_source_rows() {
        let dir = tmp();
        let a = create_scene(dir.path(), "A").unwrap();
        add_scene_source(dir.path(), &a.id, SourceRole::Screen).unwrap();
        add_scene_source(dir.path(), &a.id, SourceRole::Microphone).unwrap();
        let dup = duplicate_scene(dir.path(), &a.id).unwrap();
        assert_eq!(dup.sources.len(), 2);
        assert_eq!(dup.sources[0].role, SourceRole::Screen);
        assert_eq!(dup.sources[1].role, SourceRole::Microphone);
    }

    #[test]
    fn delete_scene_removes_it() {
        let dir = tmp();
        let a = create_scene(dir.path(), "A").unwrap();
        let _b = create_scene(dir.path(), "B").unwrap();
        delete_scene(dir.path(), &a.id).unwrap();
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "B");
    }

    #[test]
    fn delete_unknown_scene_is_an_error() {
        let dir = tmp();
        let err = delete_scene(dir.path(), "nope").unwrap_err();
        assert!(matches!(err, CoreError::SceneNotFound(_)));
    }

    #[test]
    fn add_scene_source_appends_a_placeholder_row() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        let added = add_scene_source(dir.path(), &s.id, SourceRole::Camera).unwrap();
        assert_eq!(added.role, SourceRole::Camera);
        assert_eq!(added.device.id, "default");
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list[0].sources.len(), 1);
        assert_eq!(list[0].sources[0].role, SourceRole::Camera);
    }

    #[test]
    fn add_scene_source_preserves_order_across_writes() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        for role in [
            SourceRole::Screen,
            SourceRole::Microphone,
            SourceRole::Camera,
            SourceRole::SystemAudio,
        ] {
            add_scene_source(dir.path(), &s.id, role).unwrap();
        }
        let list = list_scenes(dir.path()).unwrap();
        let roles: Vec<SourceRole> = list[0].sources.iter().map(|r| r.role).collect();
        assert_eq!(
            roles,
            vec![
                SourceRole::Screen,
                SourceRole::Microphone,
                SourceRole::Camera,
                SourceRole::SystemAudio,
            ]
        );
    }

    #[test]
    fn remove_scene_source_drops_the_row_at_that_index() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        add_scene_source(dir.path(), &s.id, SourceRole::Screen).unwrap();
        add_scene_source(dir.path(), &s.id, SourceRole::Microphone).unwrap();
        add_scene_source(dir.path(), &s.id, SourceRole::Camera).unwrap();
        remove_scene_source(dir.path(), &s.id, 1).unwrap();
        let list = list_scenes(dir.path()).unwrap();
        let roles: Vec<SourceRole> = list[0].sources.iter().map(|r| r.role).collect();
        assert_eq!(roles, vec![SourceRole::Screen, SourceRole::Camera]);
    }

    #[test]
    fn remove_scene_source_out_of_bounds_is_an_error() {
        let dir = tmp();
        let s = create_scene(dir.path(), "A").unwrap();
        let err = remove_scene_source(dir.path(), &s.id, 0).unwrap_err();
        assert!(matches!(
            err,
            CoreError::SceneSourceIndexOutOfBounds { .. }
        ));
    }

    #[test]
    fn missing_scenes_json_keeps_courses_openable() {
        // Plain Course Folder with no scenes.json — the path used by every
        // v1 Course in the wild today. read_scenes returns an empty list.
        let dir = tmp();
        let list = list_scenes(dir.path()).unwrap();
        assert!(list.is_empty());
        // And add-anything still works against the same folder; the file
        // is created lazily on first write.
        let s = create_scene(dir.path(), "First").unwrap();
        assert!(dir.path().join(SCENES_JSON).exists());
        let list = list_scenes(dir.path()).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, s.id);
    }

    #[test]
    fn malformed_scenes_json_is_an_invalid_scenes_error() {
        let dir = tmp();
        std::fs::write(dir.path().join(SCENES_JSON), b"not json").unwrap();
        let err = read_scenes(dir.path()).unwrap_err();
        assert!(matches!(err, CoreError::InvalidScenesJson { .. }));
    }
}
