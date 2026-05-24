//! In-process coordination for course windows.
//!
//! ADR-0001: within one Courseforge process, the same Course Folder is allowed
//! in at most one window. A second open attempt brings the existing window
//! forward rather than spawning a duplicate.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Default)]
pub struct CourseWindowRegistry {
    by_folder: Mutex<HashMap<PathBuf, String>>,
    by_label: Mutex<HashMap<String, PathBuf>>,
}

/// What the caller should do for an open request.
#[derive(Debug, PartialEq, Eq)]
pub enum OpenDecision {
    /// No window for this folder yet — spawn one with this label and remember it.
    Spawn { label: String },
    /// A window already exists — focus the one with this label.
    Focus { label: String },
}

impl CourseWindowRegistry {
    /// Decide whether to spawn a new window or focus an existing one for `folder`.
    /// On `Spawn`, the label is recorded so a subsequent call for the same folder
    /// returns `Focus`.
    pub fn open_or_focus(&self, folder: &Path) -> OpenDecision {
        let key = canonical_key(folder);
        let mut by_folder = self.by_folder.lock().unwrap();
        if let Some(label) = by_folder.get(&key) {
            return OpenDecision::Focus { label: label.clone() };
        }
        let label = label_for(&key, by_folder.len());
        by_folder.insert(key.clone(), label.clone());
        self.by_label.lock().unwrap().insert(label.clone(), key);
        OpenDecision::Spawn { label }
    }

    /// Forget the window for `folder` — call when a course window closes.
    pub fn release(&self, folder: &Path) {
        let key = canonical_key(folder);
        if let Some(label) = self.by_folder.lock().unwrap().remove(&key) {
            self.by_label.lock().unwrap().remove(&label);
        }
    }

    /// Look up which Course Folder a given window label is showing.
    pub fn folder_for_label(&self, label: &str) -> Option<PathBuf> {
        self.by_label.lock().unwrap().get(label).cloned()
    }
}

fn canonical_key(folder: &Path) -> PathBuf {
    // Best-effort canonicalisation so /Users/me/X and /Users/me/./X collapse.
    // Falls back to the raw path if the folder doesn't exist (tests etc.).
    std::fs::canonicalize(folder).unwrap_or_else(|_| folder.to_path_buf())
}

fn label_for(folder: &Path, seq: usize) -> String {
    // Tauri window labels must be unique strings; we use a stable-enough scheme
    // derived from the folder name plus a sequence number so it's recognisable
    // in devtools without colliding.
    let stem = folder
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("course")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>();
    format!("course-{seq}-{stem}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_open_for_a_folder_returns_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();
        let decision = reg.open_or_focus(dir.path());
        assert!(matches!(decision, OpenDecision::Spawn { .. }));
    }

    #[test]
    fn second_open_for_same_folder_returns_focus_with_same_label() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();
        let spawn = reg.open_or_focus(dir.path());
        let focus = reg.open_or_focus(dir.path());

        let label_spawn = match spawn { OpenDecision::Spawn { label } => label, _ => panic!() };
        let label_focus = match focus { OpenDecision::Focus { label } => label, _ => panic!() };
        assert_eq!(label_spawn, label_focus);
    }

    #[test]
    fn different_folders_get_distinct_labels_and_both_spawn() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();

        let da = reg.open_or_focus(a.path());
        let db = reg.open_or_focus(b.path());
        let (la, lb) = match (da, db) {
            (OpenDecision::Spawn { label: la }, OpenDecision::Spawn { label: lb }) => (la, lb),
            _ => panic!("both should spawn"),
        };
        assert_ne!(la, lb);
    }

    #[test]
    fn folder_for_label_resolves_back_to_the_folder_we_spawned_for() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();
        let label = match reg.open_or_focus(dir.path()) {
            OpenDecision::Spawn { label } => label,
            _ => panic!(),
        };
        let resolved = reg.folder_for_label(&label).unwrap();
        // canonicalize both sides for the comparison since the registry stores
        // canonical paths.
        assert_eq!(resolved, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[test]
    fn release_clears_label_lookup_too() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();
        let label = match reg.open_or_focus(dir.path()) {
            OpenDecision::Spawn { label } => label,
            _ => panic!(),
        };
        reg.release(dir.path());
        assert!(reg.folder_for_label(&label).is_none());
    }

    #[test]
    fn release_lets_the_folder_spawn_again() {
        let dir = tempfile::tempdir().unwrap();
        let reg = CourseWindowRegistry::default();
        let _ = reg.open_or_focus(dir.path());
        reg.release(dir.path());
        assert!(matches!(reg.open_or_focus(dir.path()), OpenDecision::Spawn { .. }));
    }
}
