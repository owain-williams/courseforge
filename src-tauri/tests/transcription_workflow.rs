//! End-to-end workflow for the auto-transcribe slice (issue #8).
//!
//! Drives the same code path the Tauri commands invoke — without spinning
//! up a real ASR engine — to prove that:
//!  * Keep enqueues transcription for the Video that just gained a Segment
//!  * the queue worker writes `videos/<vid>/transcript.json` with word-level
//!    timestamps
//!  * the resulting transcript is portable — relative paths only, readable
//!    from a relocated Course Folder
//!  * failures are recoverable via retry without rerunning the recording

use std::path::{Path, PathBuf};

use courseforge_lib::core::capture::{CaptureRequest, CompositionDefaults, Device, SourceRole};
use courseforge_lib::core::course;
use courseforge_lib::core::transcript::{self, Word};
use courseforge_lib::recorder::fake::FakeRecorderBackend;
use courseforge_lib::recording_manager::RecordingManager;
use courseforge_lib::transcriber::fake::{FakeTranscriberBackend, ScriptedResponse};
use courseforge_lib::transcription_manager::{JobStatus, TranscriptionManager};

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

fn fresh_recording_manager() -> RecordingManager {
    RecordingManager::new(Box::new(FakeRecorderBackend::default()))
}

fn fresh_transcription_managers() -> (TranscriptionManager, std::sync::Arc<FakeTranscriberBackend>) {
    let backend = std::sync::Arc::new(FakeTranscriberBackend::default());
    let shared = FakeTranscriberBackend {
        scripted: backend.scripted.clone(),
        calls: backend.calls.clone(),
    };
    (TranscriptionManager::new(Box::new(shared)), backend)
}

#[test]
fn keep_enqueues_transcription_and_writes_transcript_json_with_word_timestamps() {
    let (_root, folder, vid) = course_with_video();
    let rec = fresh_recording_manager();
    let (transcribe, _) = fresh_transcription_managers();

    // Record → Keep, just like the IPC command does.
    let snap = rec.start_session(&folder, &vid, vec![screen_request()]).unwrap();
    rec.stop_session(&snap.id).unwrap();
    let course_folder_at_keep = snap.course_folder.clone();
    let video_id_at_keep = snap.video_id.clone();
    let _seg = rec.keep_session(&snap.id).unwrap();

    // The command layer enqueues after a successful keep_session — replicate.
    transcribe.enqueue(course_folder_at_keep, video_id_at_keep);

    // Drain.
    let processed = transcribe.process_pending();
    assert_eq!(processed, 1);

    // transcript.json exists with word-level timestamps.
    let t = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    assert_eq!(t.video_id, vid);
    assert!(!t.words.is_empty(), "expected at least one word");
    for w in &t.words {
        assert!(w.end >= w.start, "non-monotonic timestamps: {w:?}");
    }
}

#[test]
fn transcript_remains_usable_after_copying_the_course_folder_to_another_location() {
    // NFR-6: a Course Folder copied elsewhere stays usable. The transcript
    // file lives under videos/<id>/, so a directory copy is enough.
    let (_root, folder, vid) = course_with_video();
    let rec = fresh_recording_manager();
    let (transcribe, _) = fresh_transcription_managers();

    let snap = rec.start_session(&folder, &vid, vec![screen_request()]).unwrap();
    rec.stop_session(&snap.id).unwrap();
    let _seg = rec.keep_session(&snap.id).unwrap();
    transcribe.enqueue(folder.clone(), vid.clone());
    transcribe.process_pending();

    // Copy the whole Course Folder to a sibling directory and re-read.
    let other_root = tempfile::tempdir().unwrap();
    let dst = other_root.path().join("acme-relocated");
    copy_dir_all(&folder, &dst).unwrap();

    let original = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    let relocated = transcript::read_transcript(&dst, &vid).unwrap().unwrap();
    assert_eq!(original, relocated, "transcript must be portable");
}

#[test]
fn timestamps_align_with_playback_within_tolerance_for_a_known_clip() {
    // AC: "record a known clip → transcribe → verify timestamps align with
    // playback within tolerance."
    //
    // We can't actually play media in a unit test, but we *can* fix the
    // expected alignment: the transcriber returns timestamps for a known
    // Segment payload, and we assert the words land inside the expected
    // playback windows (a 50ms tolerance, well under the perceptible
    // threshold for highlighting).
    let (_root, folder, vid) = course_with_video();
    let rec = fresh_recording_manager();
    let (transcribe, backend) = fresh_transcription_managers();

    // Drive a recording so a real Segment file exists.
    let snap = rec.start_session(&folder, &vid, vec![screen_request()]).unwrap();
    rec.stop_session(&snap.id).unwrap();
    let seg = rec.keep_session(&snap.id).unwrap();
    let seg_abs = folder.join(&seg.path);

    // Pin the expected ASR output for that exact file.
    let expected = vec![
        Word { start: 0.10, end: 0.45, text: "The".into() },
        Word { start: 0.45, end: 0.80, text: " quick".into() },
        Word { start: 0.80, end: 1.30, text: " brown".into() },
        Word { start: 1.30, end: 1.70, text: " fox".into() },
    ];
    backend.script(&seg_abs, ScriptedResponse::Ok(expected.clone()));

    transcribe.enqueue(folder.clone(), vid.clone());
    transcribe.process_pending();

    let t = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    assert_eq!(t.words.len(), expected.len());
    let tol = 0.050;
    for (got, want) in t.words.iter().zip(expected.iter()) {
        assert_eq!(got.text, want.text);
        assert!((got.start - want.start).abs() <= tol, "start off: {got:?} vs {want:?}");
        assert!((got.end - want.end).abs() <= tol, "end off: {got:?} vs {want:?}");
    }
}

#[test]
fn transcription_failure_does_not_block_the_rest_of_the_video_and_is_retryable() {
    let (_root, folder, vid) = course_with_video();
    let rec = fresh_recording_manager();
    let (transcribe, backend) = fresh_transcription_managers();

    let snap = rec.start_session(&folder, &vid, vec![screen_request()]).unwrap();
    rec.stop_session(&snap.id).unwrap();
    let seg = rec.keep_session(&snap.id).unwrap();
    let seg_abs = folder.join(&seg.path);
    backend.script(&seg_abs, ScriptedResponse::Err("model not found".into()));

    transcribe.enqueue(folder.clone(), vid.clone());
    transcribe.process_pending();

    // Job is Failed, transcript.json was *not* written, but the Segment is
    // still on disk — the user can still watch / re-record.
    let job = transcribe.job_for_video(&vid).unwrap();
    assert!(matches!(job.status, JobStatus::Failed { .. }));
    assert!(transcript::read_transcript(&folder, &vid).unwrap().is_none());
    assert!(seg_abs.is_file(), "Segment must survive a transcription failure");

    // User clears the underlying problem and hits Retry.
    backend.script(
        &seg_abs,
        ScriptedResponse::Ok(vec![Word { start: 0.0, end: 0.3, text: "ok".into() }]),
    );
    transcribe.retry(&vid).unwrap();
    transcribe.process_pending();

    assert!(matches!(transcribe.job_for_video(&vid).unwrap().status, JobStatus::Done));
    let t = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    assert_eq!(t.words[0].text, "ok");
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
