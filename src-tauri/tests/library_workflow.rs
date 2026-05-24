//! End-to-end integration tests for the Library workflow described in issue #2.
//! Each test simulates a user session through the core API — no Tauri/UI involved.

use courseforge_lib::core::{
    config::{read_config, write_config, AppConfig, CONFIG_FILENAME},
    course::{create_course, rename_course},
    library::scan_library,
};

#[test]
fn create_then_scan_then_restart_sees_the_same_courses() {
    let app_data = tempfile::tempdir().unwrap();
    let scanned_root = tempfile::tempdir().unwrap();
    let config_path = app_data.path().join(CONFIG_FILENAME);

    // First launch: pick a Scanned Root and persist it.
    write_config(
        &config_path,
        &AppConfig {
            scanned_root: Some(scanned_root.path().to_path_buf()),
        },
    )
    .unwrap();

    // Create two courses.
    create_course(scanned_root.path(), "Intro to Rust").unwrap();
    create_course(scanned_root.path(), "Advanced Topics").unwrap();

    let mut before = scan_library(scanned_root.path()).unwrap();
    before.sort_by(|a, b| a.title.cmp(&b.title));
    assert_eq!(before.len(), 2);
    assert_eq!(before[0].title, "Advanced Topics");
    assert_eq!(before[1].title, "Intro to Rust");

    // Simulate quit + relaunch — re-read config, re-scan.
    let reloaded = read_config(&config_path).unwrap();
    assert_eq!(reloaded.scanned_root.as_deref(), Some(scanned_root.path()));

    let mut after = scan_library(reloaded.scanned_root.as_ref().unwrap()).unwrap();
    after.sort_by(|a, b| a.title.cmp(&b.title));
    assert_eq!(after.len(), 2);
    assert_eq!(after[0].title, "Advanced Topics");
    assert_eq!(after[1].title, "Intro to Rust");
}

#[test]
fn rename_persists_across_restart() {
    let scanned_root = tempfile::tempdir().unwrap();
    let folder = create_course(scanned_root.path(), "Original").unwrap();

    let new_folder = rename_course(&folder, "Renamed").unwrap();
    assert!(new_folder.ends_with("renamed"));

    // "Restart" — fresh scan.
    let entries = scan_library(scanned_root.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "Renamed");
}

#[test]
fn courses_remain_after_creating_files_outside_a_course_folder() {
    let scanned_root = tempfile::tempdir().unwrap();
    create_course(scanned_root.path(), "Real Course").unwrap();
    // User drops a stray file in the Scanned Root.
    std::fs::write(scanned_root.path().join("notes.txt"), "hello").unwrap();
    // And a stray empty folder.
    std::fs::create_dir(scanned_root.path().join("scratch")).unwrap();

    let entries = scan_library(scanned_root.path()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title, "Real Course");
}
