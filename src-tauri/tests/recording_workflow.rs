//! End-to-end workflow for the recording slice (issue #7).
//!
//! Drives the same code path the Tauri commands invoke — without spinning
//! up a real recorder — to prove that:
//!  * a recorded Segment ends up on disk under `videos/<vid>/segments/`
//!  * the on-disk artifact survives a "relaunch" (re-reading via `list_segments`)
//!  * Discard cleans up the in-progress file
//!  * an orphaned `.partial.mkv` from a crash is recoverable on next launch

use std::path::Path;

use courseforge_lib::core::course;
use courseforge_lib::core::permissions::CaptureSources;
use courseforge_lib::core::segments;
use courseforge_lib::recorder::fake::FakeRecorderBackend;
use courseforge_lib::recording_manager::RecordingManager;

fn course_with_video() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let folder = course::create_course(root.path(), "Acme").unwrap();
    let m = course::add_module(&folder, "Intro").unwrap();
    let v = course::add_video(&folder, &m.id, "Welcome").unwrap();
    (root, folder, v.id)
}

fn fresh_manager() -> RecordingManager {
    RecordingManager::new(Box::new(FakeRecorderBackend::default()))
}

#[test]
fn record_keep_persists_segment_and_survives_relaunch() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();

    let snap = mgr.start_session(&folder, &vid, CaptureSources::default()).unwrap();
    mgr.stop_session(&snap.id).unwrap();
    let seg = mgr.keep_session(&snap.id).unwrap();

    // The segment file lives where the AC says it should.
    let on_disk = folder.join("videos").join(&vid).join("segments").join(format!("{}.mkv", seg.id));
    assert!(on_disk.is_file());

    // "Relaunch" — a brand-new scan finds the same segment.
    let listed = segments::list_segments(&folder, &vid).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, seg.id);
}

#[test]
fn record_discard_leaves_no_partial_behind() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();

    let snap = mgr.start_session(&folder, &vid, CaptureSources::default()).unwrap();
    mgr.stop_session(&snap.id).unwrap();
    mgr.discard_session(&snap.id).unwrap();

    let segs = segments::list_segments(&folder, &vid).unwrap();
    assert!(segs.is_empty());

    let segs_dir = folder.join("videos").join(&vid).join("segments");
    // Dir may exist (from prepare_segment_path) but must be empty.
    if segs_dir.is_dir() {
        assert!(std::fs::read_dir(&segs_dir).unwrap().next().is_none());
    }
}

#[test]
fn crashed_recording_surfaces_as_orphan_and_can_be_imported() {
    // Simulate the "OS crashed mid-recording" path: a .partial.mkv exists
    // under a video folder but the session manager doesn't know about it.
    let (_root, folder, vid) = course_with_video();
    let (orphan_id, partial) = segments::prepare_segment_path(&folder, &vid).unwrap();
    std::fs::write(&partial, b"PRETEND-MKV").unwrap();

    let orphans = segments::scan_orphans(&folder).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, orphan_id);
    assert_eq!(orphans[0].video_id, vid);

    // User picks "Import" → it becomes a normal Segment.
    let seg = segments::finalize_segment(&folder, &vid, &orphan_id).unwrap();
    assert_eq!(seg.id, orphan_id);

    // No longer an orphan.
    assert!(segments::scan_orphans(&folder).unwrap().is_empty());
    // And listed as a real segment.
    assert_eq!(segments::list_segments(&folder, &vid).unwrap().len(), 1);
}

#[test]
fn has_active_recording_gates_close_window_guard() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();
    assert!(!mgr.has_active_sessions());

    let snap = mgr.start_session(&folder, &vid, CaptureSources::default()).unwrap();
    assert!(mgr.has_active_sessions(), "must block window-close while recording");

    mgr.stop_session(&snap.id).unwrap();
    // Stopped but undecided still blocks — user must Keep/Discard first.
    assert!(mgr.has_active_sessions());

    mgr.discard_session(&snap.id).unwrap();
    assert!(!mgr.has_active_sessions());
}

#[test]
fn segments_for_different_videos_are_isolated() {
    let (_root, folder, _vid_a) = course_with_video();
    // Add a second video in the same course.
    let on_disk = course::read_course(&folder).unwrap();
    let m_id = on_disk.modules[0].id.clone();
    let vid_b = course::add_video(&folder, &m_id, "Second").unwrap().id;

    let mgr = fresh_manager();
    let s = mgr.start_session(&folder, &vid_b, CaptureSources::default()).unwrap();
    mgr.stop_session(&s.id).unwrap();
    let seg_b = mgr.keep_session(&s.id).unwrap();

    let segs_a = segments::list_segments(&folder, &on_disk.videos[0].id).unwrap();
    let segs_b = segments::list_segments(&folder, &vid_b).unwrap();
    assert!(segs_a.is_empty(), "video A should have no segments");
    assert_eq!(segs_b.len(), 1);
    assert_eq!(segs_b[0].id, seg_b.id);
}

#[test]
fn segment_path_is_relative_to_the_course_folder() {
    // ADR-0001: Course Folders are self-contained — a Segment's recorded
    // path must not include any absolute Mac-specific prefix.
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();
    let snap = mgr.start_session(&folder, &vid, CaptureSources::default()).unwrap();
    mgr.stop_session(&snap.id).unwrap();
    let seg = mgr.keep_session(&snap.id).unwrap();

    assert!(seg.path.is_relative(), "segment path must be relative, was {:?}", seg.path);
    assert_eq!(
        seg.path.iter().next().and_then(|c| c.to_str()),
        Some("videos"),
        "segment paths start at the videos/ subfolder"
    );
    // And the relative path resolves correctly under any course root.
    assert!(Path::new(&folder).join(&seg.path).is_file());
}
