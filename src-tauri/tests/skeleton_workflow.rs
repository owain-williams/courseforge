//! End-to-end integration tests for issue #3 — Module/Video CRUD inside a
//! Course. These exercise the same `course.json` writes the UI causes, then
//! re-read from disk to verify the tree round-trips exactly.

use courseforge_lib::core::course::{
    add_module, add_video, create_course, delete_module, delete_video, move_video_to_module,
    read_course, rename_module, rename_video, reorder_modules, reorder_videos_in_module,
};

#[test]
fn building_a_skeleton_round_trips_through_course_json() {
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "Guitar Course").unwrap();

    let intro = add_module(&folder, "Intro").unwrap();
    let mech = add_module(&folder, "Mechanics").unwrap();

    let v_welcome = add_video(&folder, &intro.id, "Welcome").unwrap();
    let v_what = add_video(&folder, &intro.id, "What you'll learn").unwrap();
    let v_chords = add_video(&folder, &mech.id, "Open chords").unwrap();

    // Reload as if the app had quit and re-opened the folder.
    let on_disk = read_course(&folder).unwrap();
    assert_eq!(on_disk.title, "Guitar Course");
    assert_eq!(on_disk.modules.len(), 2);
    assert_eq!(on_disk.modules[0].title, "Intro");
    assert_eq!(on_disk.modules[0].video_ids, vec![v_welcome.id.clone(), v_what.id.clone()]);
    assert_eq!(on_disk.modules[1].title, "Mechanics");
    assert_eq!(on_disk.modules[1].video_ids, vec![v_chords.id.clone()]);
    let video_titles: Vec<&str> = on_disk.videos.iter().map(|v| v.title.as_str()).collect();
    assert_eq!(video_titles, vec!["Welcome", "What you'll learn", "Open chords"]);
}

#[test]
fn the_full_set_of_edit_operations_persists_to_disk() {
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "C").unwrap();

    // Build initial tree.
    let a = add_module(&folder, "A").unwrap();
    let b = add_module(&folder, "B").unwrap();
    let v1 = add_video(&folder, &a.id, "A1").unwrap();
    let v2 = add_video(&folder, &a.id, "A2").unwrap();
    let v3 = add_video(&folder, &b.id, "B1").unwrap();

    // Rename a module and a video.
    rename_module(&folder, &a.id, "Alpha").unwrap();
    rename_video(&folder, &v2.id, "A2 renamed").unwrap();

    // Reorder modules: B before Alpha.
    reorder_modules(&folder, &[b.id.clone(), a.id.clone()]).unwrap();

    // Reorder videos inside Alpha: A2 first.
    reorder_videos_in_module(&folder, &a.id, &[v2.id.clone(), v1.id.clone()]).unwrap();

    // Move a video to another module — JSON edit only.
    move_video_to_module(&folder, &v1.id, &b.id, 0).unwrap();

    // Delete a single video (not a cascade).
    delete_video(&folder, &v3.id).unwrap();

    // Reload and assert the whole tree.
    let on_disk = read_course(&folder).unwrap();
    assert_eq!(on_disk.modules.iter().map(|m| m.title.as_str()).collect::<Vec<_>>(),
               vec!["B", "Alpha"]);

    let b_after = on_disk.modules.iter().find(|m| m.id == b.id).unwrap();
    let a_after = on_disk.modules.iter().find(|m| m.id == a.id).unwrap();
    assert_eq!(b_after.video_ids, vec![v1.id.clone()]);
    assert_eq!(a_after.video_ids, vec![v2.id.clone()]);

    let video_titles: Vec<&str> = on_disk.videos.iter().map(|v| v.title.as_str()).collect();
    // v3 is gone; v2 is renamed.
    assert!(!video_titles.contains(&"B1"));
    assert!(video_titles.contains(&"A2 renamed"));
}

#[test]
fn deleting_a_module_cascades_to_its_videos_on_disk() {
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "C").unwrap();
    let keep = add_module(&folder, "Keep").unwrap();
    let drop = add_module(&folder, "Drop").unwrap();
    let v_kept = add_video(&folder, &keep.id, "K").unwrap();
    let _v_gone = add_video(&folder, &drop.id, "G").unwrap();

    delete_module(&folder, &drop.id).unwrap();

    let on_disk = read_course(&folder).unwrap();
    assert_eq!(on_disk.modules.len(), 1);
    assert_eq!(on_disk.modules[0].id, keep.id);
    assert_eq!(on_disk.videos.len(), 1);
    assert_eq!(on_disk.videos[0].id, v_kept.id);
}

#[test]
fn moving_a_video_between_modules_does_not_touch_on_disk_video_folder() {
    // ADR-0001: per-Video subfolders are named by opaque IDs and live in a
    // flat `videos/` directory; moving a Video between Modules is a JSON edit
    // only.
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "C").unwrap();
    let src = add_module(&folder, "Src").unwrap();
    let dst = add_module(&folder, "Dst").unwrap();
    let v = add_video(&folder, &src.id, "V").unwrap();

    // Simulate the on-disk video folder existing (e.g. from a prior recording).
    let video_folder = folder.join("videos").join(&v.id);
    std::fs::create_dir_all(&video_folder).unwrap();
    std::fs::write(video_folder.join("segments.placeholder"), b"x").unwrap();

    move_video_to_module(&folder, &v.id, &dst.id, 0).unwrap();

    // The folder is still there, untouched.
    assert!(video_folder.is_dir());
    assert!(video_folder.join("segments.placeholder").is_file());

    let on_disk = read_course(&folder).unwrap();
    assert_eq!(on_disk.modules.iter().find(|m| m.id == src.id).unwrap().video_ids, Vec::<String>::new());
    assert_eq!(on_disk.modules.iter().find(|m| m.id == dst.id).unwrap().video_ids, vec![v.id]);
}
