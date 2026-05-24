//! End-to-end integration tests for issue #4 — Library ops: Pin / Remove
//! from Library / Move to Trash. Each test simulates a user session through
//! the core API.

use courseforge_lib::core::{
    config::{read_config, write_config, AppConfig, CONFIG_FILENAME},
    course::create_course,
    library::{
        library_view, move_course_folder_to_trash, pin_folder, unpin_folder, CourseSource,
    },
};

#[test]
fn pin_then_relaunch_shows_the_pinned_course_alongside_scanned_root() {
    let app_data = tempfile::tempdir().unwrap();
    let scanned_root = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let config_path = app_data.path().join(CONFIG_FILENAME);

    // First launch: pick Scanned Root; create one Course inside it.
    write_config(
        &config_path,
        &AppConfig {
            scanned_root: Some(scanned_root.path().to_path_buf()),
            pinned_folders: Vec::new(),
            ignored_folders: Vec::new(),
        },
    )
    .unwrap();
    create_course(scanned_root.path(), "In Root").unwrap();

    // User pins a Course Folder that lives outside the Scanned Root.
    let pinned = create_course(elsewhere.path(), "Outside Root").unwrap();
    let mut cfg = read_config(&config_path).unwrap();
    pin_folder(&mut cfg, &pinned).unwrap();
    write_config(&config_path, &cfg).unwrap();

    // Simulate quit + relaunch — re-read config, re-derive the Library.
    let reloaded = read_config(&config_path).unwrap();
    let mut entries = library_view(&reloaded).unwrap();
    entries.sort_by(|a, b| a.title.cmp(&b.title));

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].title, "In Root");
    assert_eq!(entries[0].source, CourseSource::Scanned);
    assert_eq!(entries[1].title, "Outside Root");
    assert_eq!(entries[1].source, CourseSource::Pinned);
}

#[test]
fn unpin_removes_from_library_without_touching_the_folder_on_disk() {
    let app_data = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let config_path = app_data.path().join(CONFIG_FILENAME);

    let pinned = create_course(elsewhere.path(), "Keep My Bytes").unwrap();
    let mut cfg = AppConfig {
        scanned_root: None,
        pinned_folders: Vec::new(),
        ignored_folders: Vec::new(),
    };
    pin_folder(&mut cfg, &pinned).unwrap();
    write_config(&config_path, &cfg).unwrap();
    assert_eq!(library_view(&cfg).unwrap().len(), 1);

    // User picks "Remove from Library" on the pinned entry.
    let mut cfg = read_config(&config_path).unwrap();
    let target = cfg.pinned_folders[0].clone();
    let removed = unpin_folder(&mut cfg, &target);
    write_config(&config_path, &cfg).unwrap();

    assert!(removed);
    assert_eq!(library_view(&cfg).unwrap().len(), 0);
    // The folder on disk is untouched.
    assert!(pinned.is_dir());
    assert!(pinned.join("course.json").is_file());
}

#[test]
fn move_to_trash_removes_a_scanned_root_course_from_library_and_disk() {
    let app_data = tempfile::tempdir().unwrap();
    let scanned_root = tempfile::tempdir().unwrap();
    let config_path = app_data.path().join(CONFIG_FILENAME);

    write_config(
        &config_path,
        &AppConfig {
            scanned_root: Some(scanned_root.path().to_path_buf()),
            pinned_folders: Vec::new(),
            ignored_folders: Vec::new(),
        },
    )
    .unwrap();
    let folder = create_course(scanned_root.path(), "Goodbye").unwrap();

    let before = library_view(&read_config(&config_path).unwrap()).unwrap();
    assert_eq!(before.len(), 1);

    move_course_folder_to_trash(&folder).unwrap();

    let after = library_view(&read_config(&config_path).unwrap()).unwrap();
    assert!(after.is_empty(), "scan must no longer see the trashed folder");
    assert!(!folder.exists(), "folder must be gone from its original location");
}

#[test]
fn missing_pinned_folder_is_flagged_in_library_but_not_auto_removed() {
    let app_data = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let config_path = app_data.path().join(CONFIG_FILENAME);

    let pinned = create_course(elsewhere.path(), "Will Vanish").unwrap();
    let mut cfg = AppConfig {
        scanned_root: None,
        pinned_folders: Vec::new(),
        ignored_folders: Vec::new(),
    };
    pin_folder(&mut cfg, &pinned).unwrap();
    write_config(&config_path, &cfg).unwrap();

    // Folder disappears between launches (user deleted it in Finder).
    std::fs::remove_dir_all(&pinned).unwrap();

    let reloaded = read_config(&config_path).unwrap();
    let entries = library_view(&reloaded).unwrap();

    assert_eq!(entries.len(), 1);
    assert!(entries[0].missing);
    assert_eq!(entries[0].source, CourseSource::Pinned);
    // Config still holds the pin — the user gets to decide.
    assert_eq!(reloaded.pinned_folders.len(), 1);
}
