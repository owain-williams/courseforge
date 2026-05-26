//! End-to-end workflow for the export slice (issue #10), using the fake
//! exporter so it runs on every platform without ffmpeg installed.
//!
//! Drives the same code path the Tauri commands invoke to prove:
//!   * record → transcribe → cut → export produces an MP4 + SRT at the
//!     default destination (`<Course Folder>/exports/<video-id>/`)
//!   * the source Segment file is never modified by export (AC: "Export
//!     does not modify source files or the EDL")
//!   * the EDL file is never modified by export
//!   * the SRT timecodes match the *edited* timeline (AC: ".srt parses and
//!     aligns" — see also the real-ffmpeg integration test for the
//!     duration assertion the AC explicitly calls out)
//!
//! Real ffmpeg duration / playback verification lives in
//! `export_real_ffmpeg.rs`, gated behind `--ignored` so contributors
//! without ffmpeg installed still get a green run.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use courseforge_lib::core::capture::{CaptureRequest, CompositionDefaults, Device, SourceRole};
use courseforge_lib::core::course;
use courseforge_lib::core::edits;
use courseforge_lib::core::transcript::Word;
use courseforge_lib::export_manager::{ExportManager, ExportStatus};
use courseforge_lib::exporter::fake::FakeExporter;
use courseforge_lib::recorder::fake::FakeRecorderBackend;
use courseforge_lib::recording_manager::RecordingManager;
use courseforge_lib::transcriber::fake::{FakeTranscriberBackend, ScriptedResponse};
use courseforge_lib::transcription_manager::TranscriptionManager;

fn screen_request() -> CaptureRequest {
    CaptureRequest {
        role: SourceRole::Screen,
        device: Device { id: "default".into(), label: "Main Display".into() },
        defaults: CompositionDefaults::default(),
    }
}

fn course_with_video() -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let folder = course::create_course(root.path(), "Acme").unwrap();
    let m = course::add_module(&folder, "Intro").unwrap();
    let v = course::add_video(&folder, &m.id, "Welcome").unwrap();
    (root, folder, v.id)
}

fn record_and_transcribe(folder: &Path, video_id: &str, words: Vec<Word>) -> PathBuf {
    let rec = RecordingManager::new(Box::new(FakeRecorderBackend::default()));
    let backend = Arc::new(FakeTranscriberBackend::default());
    let shared = FakeTranscriberBackend {
        scripted: backend.scripted.clone(),
        calls: backend.calls.clone(),
    };
    let tr = TranscriptionManager::new(Box::new(shared));

    let snap = rec
        .start_session(folder, video_id, vec![screen_request()])
        .unwrap();
    rec.stop_session(&snap.id).unwrap();
    let seg = rec.keep_session(&snap.id).unwrap();
    let seg_abs = folder.join(&seg.path);
    backend.script(&seg_abs, ScriptedResponse::Ok(words));

    tr.enqueue(folder.to_path_buf(), video_id.to_string());
    tr.process_pending();
    seg_abs
}

fn five_words() -> Vec<Word> {
    vec![
        Word { start: 0.5, end: 1.0, text: "Hello".into() },
        Word { start: 1.0, end: 1.5, text: " brave".into() },
        Word { start: 1.5, end: 2.0, text: " new".into() },
        Word { start: 2.0, end: 2.5, text: " world".into() },
        Word { start: 3.0, end: 3.5, text: " indeed".into() },
    ]
}

#[test]
fn record_cut_export_writes_mp4_and_srt_to_default_destination() {
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_words());

    edits::append_cut(&folder, &vid, 1.0, 2.5).unwrap();

    let mgr = Arc::new(ExportManager::new(Box::new(FakeExporter::default())));
    let job = mgr.start_export(&folder, &vid, None).unwrap();
    let (mp4, srt) = match job.status {
        ExportStatus::Done { mp4, srt } => (mp4, srt),
        other => panic!("expected Done, got {other:?}"),
    };
    let expected_dir = folder.join("exports").join(&vid);
    assert_eq!(mp4.parent(), Some(expected_dir.as_path()));
    assert!(mp4.is_file());
    assert!(srt.is_file());
}

#[test]
fn export_does_not_modify_the_source_segment_or_the_edl() {
    let (_root, folder, vid) = course_with_video();
    let seg_path = record_and_transcribe(&folder, &vid, five_words());
    edits::append_cut(&folder, &vid, 1.0, 1.5).unwrap();

    let seg_before = std::fs::read(&seg_path).unwrap();
    let edits_before = std::fs::read(edits::edits_path(&folder, &vid)).unwrap();

    let mgr = Arc::new(ExportManager::new(Box::new(FakeExporter::default())));
    mgr.start_export(&folder, &vid, None).unwrap();

    assert_eq!(std::fs::read(&seg_path).unwrap(), seg_before);
    assert_eq!(
        std::fs::read(edits::edits_path(&folder, &vid)).unwrap(),
        edits_before
    );
}

#[test]
fn srt_uses_the_edited_timeline_not_the_source() {
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_words());
    // Cut 1.0–2.5: removes " brave", " new", " world". Remaining words:
    // "Hello" (source 0.5–1.0 → edited 0.5–1.0) and " indeed" (source
    // 3.0–3.5 → after a 1.5s cut, edited 1.5–2.0).
    edits::append_cut(&folder, &vid, 1.0, 2.5).unwrap();

    let mgr = Arc::new(ExportManager::new(Box::new(FakeExporter::default())));
    let job = mgr.start_export(&folder, &vid, None).unwrap();
    let srt_path = match job.status {
        ExportStatus::Done { srt, .. } => srt,
        other => panic!("expected Done, got {other:?}"),
    };
    let srt = std::fs::read_to_string(&srt_path).unwrap();

    // First kept word stays at 0.5s (no cut before it).
    assert!(srt.contains("00:00:00,500"), "expected first cue at 0.500s, got:\n{srt}");
    // Second kept word remapped to 1.5s (after the 1.5s cut).
    assert!(srt.contains("00:00:01,500"), "expected remapped second cue at 1.500s, got:\n{srt}");
    // Cut words don't appear.
    assert!(!srt.contains("brave"));
    assert!(!srt.contains("new"));
    assert!(!srt.contains("world"));
}

#[test]
fn export_destination_is_user_overridable() {
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_words());
    let custom = tempfile::tempdir().unwrap();

    let mgr = Arc::new(ExportManager::new(Box::new(FakeExporter::default())));
    let job = mgr
        .start_export(&folder, &vid, Some(custom.path().to_path_buf()))
        .unwrap();
    match job.status {
        ExportStatus::Done { mp4, srt } => {
            assert!(mp4.starts_with(custom.path()));
            assert!(srt.starts_with(custom.path()));
        }
        other => panic!("expected Done, got {other:?}"),
    }
}
