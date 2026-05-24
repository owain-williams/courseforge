//! End-to-end integration test for issue #5 — workflow board persistence.
//! Mirrors the user-visible flow: create Videos, drag them across columns
//! (i.e. call `set_video_state`), reload the Course off disk, and verify
//! every transition stuck. Also exercises the user-editable state list
//! (add / rename / reorder / remove-with-fallback).

use courseforge_lib::core::course::{
    add_module, add_video, add_workflow_state, create_course, read_course,
    remove_workflow_state, rename_workflow_state, reorder_workflow_states, set_video_state,
};

#[test]
fn videos_remember_their_workflow_state_across_reloads() {
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "Guitar Course").unwrap();

    // Brand-new Course ships with the canonical default workflow.
    let initial = read_course(&folder).unwrap();
    let default_ids: Vec<&str> =
        initial.workflow_states.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(
        default_ids,
        vec!["needs-recording", "needs-editing", "needs-intro", "needs-uploading", "done"]
    );

    // Build a small skeleton.
    let m = add_module(&folder, "Intro").unwrap();
    let welcome = add_video(&folder, &m.id, "Welcome").unwrap();
    let outline = add_video(&folder, &m.id, "What you'll learn").unwrap();
    let demo = add_video(&folder, &m.id, "Demo").unwrap();

    // New Videos start in the first column.
    assert_eq!(welcome.state_id.as_deref(), Some("needs-recording"));

    // Drag each Video to a different column.
    set_video_state(&folder, &welcome.id, "done").unwrap();
    set_video_state(&folder, &outline.id, "needs-editing").unwrap();
    set_video_state(&folder, &demo.id, "needs-uploading").unwrap();

    // Reload as if the app had quit and re-opened the Course Folder.
    let on_disk = read_course(&folder).unwrap();
    let by_id = |id: &str| {
        on_disk
            .videos
            .iter()
            .find(|v| v.id == id)
            .unwrap()
            .state_id
            .clone()
    };
    assert_eq!(by_id(&welcome.id).as_deref(), Some("done"));
    assert_eq!(by_id(&outline.id).as_deref(), Some("needs-editing"));
    assert_eq!(by_id(&demo.id).as_deref(), Some("needs-uploading"));
}

#[test]
fn user_can_customise_the_state_list_and_changes_survive_reload() {
    let root = tempfile::tempdir().unwrap();
    let folder = create_course(root.path(), "C").unwrap();
    let m = add_module(&folder, "M").unwrap();
    let v = add_video(&folder, &m.id, "V").unwrap();

    // Add a new state, rename an existing one, reorder.
    let review = add_workflow_state(&folder, "Awaiting Review").unwrap();
    rename_workflow_state(&folder, "done", "Shipped").unwrap();
    reorder_workflow_states(
        &folder,
        &[
            "needs-recording".into(),
            "needs-editing".into(),
            review.id.clone(),
            "needs-intro".into(),
            "needs-uploading".into(),
            "done".into(),
        ],
    )
    .unwrap();

    // Park the Video in the new state, then remove that state and verify
    // the Video gets reassigned to the user-chosen fallback.
    set_video_state(&folder, &v.id, &review.id).unwrap();
    remove_workflow_state(&folder, &review.id, "needs-editing").unwrap();

    let on_disk = read_course(&folder).unwrap();
    let names: Vec<&str> = on_disk.workflow_states.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["Needs Recording", "Needs Editing", "Needs Intro", "Needs Uploading", "Shipped"]
    );
    assert_eq!(on_disk.videos[0].state_id.as_deref(), Some("needs-editing"));
}
