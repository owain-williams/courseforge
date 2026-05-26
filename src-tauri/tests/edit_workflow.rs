//! End-to-end workflow for the transcript-driven cut slice (issue #9).
//!
//! Drives the same code path the Tauri commands invoke to prove:
//!   * record → transcribe → cut writes an EDL alongside the Video
//!   * `edits.json` survives "relaunch" (re-reading from disk) with the
//!     same cuts and undo/redo cursor
//!   * the source Segment file is never modified (FR-5.4 non-destructive)
//!   * unlimited undo/redo works across "sessions" — i.e. is fully
//!     reconstructable from the persisted log (FR-5.5, NFR-5)
//!   * the Course Folder remains portable: an EDL written on one Mac is
//!     identically readable from a copied folder on another (NFR-6)

use std::path::{Path, PathBuf};

use courseforge_lib::core::course;
use courseforge_lib::core::edits;
use courseforge_lib::core::permissions::CaptureSources;
use courseforge_lib::core::transcript::{self, Word};
use courseforge_lib::recorder::fake::FakeRecorderBackend;
use courseforge_lib::recording_manager::RecordingManager;
use courseforge_lib::remuxer::fake::FakeRemuxer;
use courseforge_lib::transcriber::fake::{FakeTranscriberBackend, ScriptedResponse};
use courseforge_lib::transcription_manager::TranscriptionManager;

fn course_with_video() -> (tempfile::TempDir, PathBuf, String) {
    let root = tempfile::tempdir().unwrap();
    let folder = course::create_course(root.path(), "Acme").unwrap();
    let m = course::add_module(&folder, "Intro").unwrap();
    let v = course::add_video(&folder, &m.id, "Welcome").unwrap();
    (root, folder, v.id)
}

fn fresh_recording_manager() -> RecordingManager {
    RecordingManager::new(
        Box::new(FakeRecorderBackend::default()),
        Box::new(FakeRemuxer::default()),
    )
}

fn fresh_transcription_managers() -> (TranscriptionManager, std::sync::Arc<FakeTranscriberBackend>) {
    let backend = std::sync::Arc::new(FakeTranscriberBackend::default());
    let shared = FakeTranscriberBackend {
        scripted: backend.scripted.clone(),
        calls: backend.calls.clone(),
    };
    (TranscriptionManager::new(Box::new(shared)), backend)
}

/// Drive a record → keep → transcribe sequence and return the on-disk
/// Segment path so the test can also assert the file is left untouched by
/// any subsequent EDL operation.
fn record_and_transcribe(
    folder: &Path,
    video_id: &str,
    words: Vec<Word>,
) -> PathBuf {
    let rec = fresh_recording_manager();
    let (transcribe, backend) = fresh_transcription_managers();

    let snap = rec
        .start_session(folder, video_id, CaptureSources::default())
        .unwrap();
    rec.stop_session(&snap.id).unwrap();
    let seg = rec.keep_session(&snap.id).unwrap();
    let seg_abs = folder.join(&seg.path);
    backend.script(&seg_abs, ScriptedResponse::Ok(words));

    transcribe.enqueue(folder.to_path_buf(), video_id.to_string());
    let _ = transcribe.process_pending();
    seg_abs
}

fn five_word_transcript() -> Vec<Word> {
    // Word timings carved up so the test cuts hit "obvious" boundaries —
    // 1.0-1.5s, 3.0-3.5s, etc.
    vec![
        Word { start: 0.5, end: 1.0, text: "Hello".into() },
        Word { start: 1.0, end: 1.5, text: " brave".into() },
        Word { start: 1.5, end: 2.0, text: " new".into() },
        Word { start: 2.0, end: 2.5, text: " world".into() },
        Word { start: 3.0, end: 3.5, text: " indeed".into() },
    ]
}

#[test]
fn cutting_a_word_range_writes_edits_json_and_leaves_the_segment_untouched() {
    // AC: "Delete creates a cut in edits.json; source Segment files are
    // untouched (FR-5.4 non-destructive)."
    let (_root, folder, vid) = course_with_video();
    let seg_path = record_and_transcribe(&folder, &vid, five_word_transcript());

    // Snapshot the Segment bytes so we can prove they don't change.
    let bytes_before = std::fs::read(&seg_path).unwrap();

    // Cut the words "brave new world" — transcript spans 1.0s → 2.5s.
    let state = edits::append_cut(&folder, &vid, 1.0, 2.5).unwrap();
    assert_eq!(state.cuts.len(), 1);
    assert!(state.can_undo);
    assert!(!state.can_redo);

    // edits.json now exists.
    let edits_path = edits::edits_path(&folder, &vid);
    assert!(edits_path.is_file(), "edits.json should be persisted");

    // Source Segment is byte-for-byte unchanged.
    let bytes_after = std::fs::read(&seg_path).unwrap();
    assert_eq!(bytes_before, bytes_after, "segment must never be modified");

    // Transcript file is also untouched — cuts live separately.
    let t = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    assert_eq!(t.words.len(), 5);
}

#[test]
fn edits_persist_across_relaunch_with_full_command_log_preserved() {
    // AC: "EDL persists across relaunch and is editable on a different Mac
    // after copying the Course Folder."
    //
    // We simulate relaunch by re-reading the EDL fresh and checking its
    // derived state matches what the in-session API returned.
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_word_transcript());

    let s1 = edits::append_cut(&folder, &vid, 1.0, 1.5).unwrap();
    let s2 = edits::append_cut(&folder, &vid, 2.0, 2.5).unwrap();
    let s3 = edits::append_undo(&folder, &vid).unwrap();
    assert_eq!(s3.cuts.len(), 1);
    assert!(s3.can_redo);

    // "Relaunch" — discard everything but the folder and re-read.
    let reloaded = edits::current_state(&folder, &vid).unwrap();
    assert_eq!(reloaded, s3);

    // And a redo after relaunch restores the redone cut.
    let s4 = edits::append_redo(&folder, &vid).unwrap();
    assert_eq!(s4.cuts.len(), 2);
    let _ = (s1, s2);
}

#[test]
fn edits_remain_usable_after_copying_the_course_folder_to_another_location() {
    // NFR-6: Course Folders are portable. An EDL written on one machine
    // must read identically from a copy on another (no absolute paths, no
    // machine-state baked in).
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_word_transcript());

    edits::append_cut(&folder, &vid, 1.0, 1.5).unwrap();
    edits::append_cut(&folder, &vid, 3.0, 3.5).unwrap();

    let other_root = tempfile::tempdir().unwrap();
    let dst = other_root.path().join("acme-relocated");
    copy_dir_all(&folder, &dst).unwrap();

    let here = edits::current_state(&folder, &vid).unwrap();
    let there = edits::current_state(&dst, &vid).unwrap();
    assert_eq!(here, there, "edits must be portable across machines");
    assert_eq!(here.cuts.len(), 2);
}

#[test]
fn full_workflow_record_transcribe_cut_relaunch_verify() {
    // The AC integration test: record → transcribe → cut several ranges →
    // relaunch → verify EDL + playback match.
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_word_transcript());

    // Cut three distinct ranges.
    let ranges = [(0.5, 1.0), (1.5, 2.0), (3.0, 3.5)];
    for (s, e) in ranges {
        edits::append_cut(&folder, &vid, s, e).unwrap();
    }

    // "Relaunch" by re-reading the persisted log.
    let state = edits::current_state(&folder, &vid).unwrap();
    assert_eq!(state.cuts.len(), 3);
    // Cuts come back sorted by start_sec — playback can binary-search
    // them without re-sorting.
    let starts: Vec<_> = state.cuts.iter().map(|c| c.start_sec).collect();
    assert_eq!(starts, vec![0.5, 1.5, 3.0]);

    // Verify the cuts correspond to the right transcript words. This is
    // the "playback match" assertion — the player would skip any time
    // range covered by a cut, which is exactly the words we picked.
    let t = transcript::read_transcript(&folder, &vid).unwrap().unwrap();
    let cut_word_texts: Vec<_> = t
        .words
        .iter()
        .filter(|w| state.cuts.iter().any(|c| w.start >= c.start_sec && w.end <= c.end_sec))
        .map(|w| w.text.as_str())
        .collect();
    assert_eq!(cut_word_texts, vec!["Hello", " new", " indeed"]);
}

#[test]
fn undo_redo_survives_relaunch_and_remains_unbounded() {
    // FR-5.5: unlimited undo/redo within an editing session. Across a
    // simulated relaunch the cursor position is reconstructed faithfully
    // from the persisted command log.
    let (_root, folder, vid) = course_with_video();
    let _seg = record_and_transcribe(&folder, &vid, five_word_transcript());

    for i in 0..50 {
        let s = i as f64 * 0.1;
        edits::append_cut(&folder, &vid, s, s + 0.05).unwrap();
    }
    // Undo half of them.
    for _ in 0..25 {
        edits::append_undo(&folder, &vid).unwrap();
    }

    let mid = edits::current_state(&folder, &vid).unwrap();
    assert_eq!(mid.cuts.len(), 25);
    assert!(mid.can_undo);
    assert!(mid.can_redo);

    // Relaunch — re-read fresh.
    let reloaded = edits::current_state(&folder, &vid).unwrap();
    assert_eq!(reloaded, mid);

    // Continue with redo from the reloaded state.
    for _ in 0..25 {
        edits::append_redo(&folder, &vid).unwrap();
    }
    let end = edits::current_state(&folder, &vid).unwrap();
    assert_eq!(end.cuts.len(), 50);
    assert!(!end.can_redo);
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
