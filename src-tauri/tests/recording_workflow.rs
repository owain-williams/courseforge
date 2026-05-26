//! End-to-end workflow for the recording slice (issue #7, updated for #30).
//!
//! Drives the same code path the Tauri commands invoke — without spinning
//! up a real recorder — to prove that:
//!  * a recorded Segment ends up on disk under `videos/<vid>/segments/` as
//!    `.mov` with a sidecar JSON
//!  * the on-disk artifact survives a "relaunch" (re-reading via `list_segments`)
//!  * Discard cleans up the in-progress file
//!  * an orphaned partial from a crash is recoverable on next launch (both
//!    the new `.partial.mov` shape and the legacy `.partial.mkv`)

use std::path::Path;

use courseforge_lib::core::capture::{
    CaptureRequest, CompositionDefaults, Device, SourceRole,
};
use courseforge_lib::core::course;
use courseforge_lib::core::segments::{self, LEGACY_PARTIAL_SUFFIX};
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

fn screen_request() -> CaptureRequest {
    CaptureRequest {
        role: SourceRole::Screen,
        device: Device {
            id: "default".into(),
            label: "Main Display".into(),
        },
        defaults: CompositionDefaults::default(),
    }
}

#[test]
fn record_keep_persists_mov_with_sidecar_and_survives_relaunch() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();

    let snap = mgr
        .start_session(&folder, &vid, vec![screen_request()])
        .unwrap();
    mgr.stop_session(&snap.id).unwrap();
    let seg = mgr.keep_session(&snap.id).unwrap();

    // The segment file lives where the AC says it should — `.mov` (no
    // remux). Sidecar lands next to it.
    let on_disk = folder
        .join("videos")
        .join(&vid)
        .join("segments")
        .join(format!("{}.mov", seg.id));
    assert!(on_disk.is_file());
    let sidecar = segments::read_sidecar(&folder, &vid, &seg.id).unwrap();
    assert!(sidecar.is_some(), "keep must write a sidecar");

    // "Relaunch" — a brand-new scan finds the same segment.
    let listed = segments::list_segments(&folder, &vid).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, seg.id);
}

#[test]
fn record_discard_leaves_no_partial_behind() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();

    let snap = mgr
        .start_session(&folder, &vid, vec![screen_request()])
        .unwrap();
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
fn crashed_partial_mov_surfaces_as_orphan_and_imports_with_sidecar() {
    let (_root, folder, vid) = course_with_video();
    let (orphan_id, partial) = segments::prepare_segment_path(&folder, &vid).unwrap();
    std::fs::write(&partial, b"RECOVERED-MOV").unwrap();

    let orphans = segments::scan_orphans(&folder).unwrap();
    assert_eq!(orphans.len(), 1);
    assert_eq!(orphans[0].id, orphan_id);

    let mgr = fresh_manager();
    let seg = mgr.adopt_orphan(&folder, &vid, &orphan_id).unwrap();
    assert_eq!(seg.id, orphan_id);
    assert!(seg.path.to_string_lossy().ends_with(".mov"));

    // Adopted orphans get a sidecar marked endedReason=crashed.
    let sidecar = segments::read_sidecar(&folder, &vid, &orphan_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        sidecar.ended_reason,
        courseforge_lib::core::capture::EndedReason::Crashed
    );

    assert!(segments::scan_orphans(&folder).unwrap().is_empty());
    assert_eq!(segments::list_segments(&folder, &vid).unwrap().len(), 1);
}

#[test]
fn legacy_partial_mkv_surfaces_as_orphan_for_v1_back_compat() {
    // v1 Course Folders from before Phase 1 left `.partial.mkv` orphans
    // when capture crashed. AC: "v1 .partial.mkv orphans remain importable
    // through the existing orphan flow (this slice does not change v1
    // orphan behaviour; Phase 7 handles polish)."
    //
    // This test asserts the *discovery* half (scan_orphans picks them up).
    // The macOS-only import path shells out to real ffmpeg which we don't
    // exercise here — fabricating a real `.partial.mkv` byte stream for a
    // unit test is overkill; the inline `segments::adopt_orphan` already
    // covers the rename / fallback shape via its own unit tests, and the
    // macOS-gated integration test (`#[ignore]`) covers the real path.
    let (_root, folder, vid) = course_with_video();
    let segs_dir = folder.join("videos").join(&vid).join("segments");
    std::fs::create_dir_all(&segs_dir).unwrap();
    let legacy = segs_dir.join(format!("legacy.{LEGACY_PARTIAL_SUFFIX}"));
    std::fs::write(&legacy, b"v1-mkv-bytes").unwrap();

    let orphans = segments::scan_orphans(&folder).unwrap();
    assert!(
        orphans.iter().any(|o| o.id == "legacy"),
        "scan_orphans must still pick up .partial.mkv from v1 Course Folders"
    );
}

#[test]
fn has_active_recording_gates_close_window_guard() {
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();
    assert!(!mgr.has_active_sessions());

    let snap = mgr
        .start_session(&folder, &vid, vec![screen_request()])
        .unwrap();
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
    let s = mgr
        .start_session(&folder, &vid_b, vec![screen_request()])
        .unwrap();
    mgr.stop_session(&s.id).unwrap();
    let seg_b = mgr.keep_session(&s.id).unwrap();

    let segs_a = segments::list_segments(&folder, &on_disk.videos[0].id).unwrap();
    let segs_b = segments::list_segments(&folder, &vid_b).unwrap();
    assert!(segs_a.is_empty(), "video A should have no segments");
    assert_eq!(segs_b.len(), 1);
    assert_eq!(segs_b[0].id, seg_b.id);
}

/// Issue #30 AC integration test. Records ~3 seconds of screen+mic via the
/// real `SckMacBackend`, then asserts the `.mov` lands at the right path,
/// the sidecar JSON validates against the new schema, and `ffprobe`
/// reports a positive duration with both video and audio tracks.
///
/// `#[ignore]` because the host needs Screen Recording + Microphone
/// permission granted to the Cargo test binary (CI can't grant those).
/// Run locally with:
///
/// ```text
/// cargo test -p courseforge --test recording_workflow -- \
///     --ignored mac_in_process_recorder_writes_a_playable_screen_plus_mic_mov
/// ```
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn mac_in_process_recorder_writes_a_playable_screen_plus_mic_mov() {
    use std::time::Duration;

    use courseforge_lib::recorder::sck_mac::SckMacBackend;

    let (_root, folder, vid) = course_with_video();
    let mgr = RecordingManager::new(Box::new(SckMacBackend::default()));

    let requests = vec![
        screen_request(),
        CaptureRequest {
            role: SourceRole::Microphone,
            device: Device {
                id: "default".into(),
                label: "Default Microphone".into(),
            },
            defaults: CompositionDefaults::default(),
        },
    ];

    let snap = mgr.start_session(&folder, &vid, requests).unwrap();
    std::thread::sleep(Duration::from_secs(3));
    mgr.stop_session(&snap.id).unwrap();
    let seg = mgr.keep_session(&snap.id).unwrap();

    // .mov on disk at the canonical path.
    let mov = folder.join(&seg.path);
    assert!(mov.is_file(), "expected .mov at {mov:?}");
    assert!(mov.extension().and_then(|s| s.to_str()) == Some("mov"));

    // Sidecar validates and carries the right shape.
    let sidecar = segments::read_sidecar(&folder, &vid, &seg.id).unwrap()
        .expect("sidecar must be written on Keep");
    assert_eq!(sidecar.schema_version, 1);
    assert!(!sidecar.take_id.is_empty());
    assert_eq!(sidecar.ended_reason, courseforge_lib::core::capture::EndedReason::Normal);

    // ffprobe reports positive duration with at least one video and one audio stream.
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "stream=codec_type",
            "-of", "default=nw=1:nk=1",
        ])
        .arg(&mov)
        .output()
        .expect("ffprobe must be installed to verify the .mov");
    assert!(probe.status.success(), "ffprobe failed: {}",
        String::from_utf8_lossy(&probe.stderr));
    let kinds = String::from_utf8_lossy(&probe.stdout);
    assert!(kinds.contains("video"), "no video track in {mov:?}; ffprobe: {kinds}");
    assert!(kinds.contains("audio"), "no audio track in {mov:?}; ffprobe: {kinds}");

    let duration = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=nw=1:nk=1",
        ])
        .arg(&mov)
        .output()
        .expect("ffprobe duration query failed to spawn");
    let s = String::from_utf8_lossy(&duration.stdout).trim().to_string();
    let secs: f64 = s.parse().unwrap_or(0.0);
    assert!(secs > 1.0, "expected ≥1s duration, ffprobe reported {s:?}");
}

#[test]
fn segment_path_is_relative_to_the_course_folder() {
    // ADR-0001: Course Folders are self-contained — a Segment's recorded
    // path must not include any absolute Mac-specific prefix.
    let (_root, folder, vid) = course_with_video();
    let mgr = fresh_manager();
    let snap = mgr
        .start_session(&folder, &vid, vec![screen_request()])
        .unwrap();
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
