//! End-to-end workflow for the export slice (issue #10) using **real**
//! ffmpeg + ffprobe.
//!
//! Skipped unless `ffmpeg` and `ffprobe` are on `PATH` so contributors
//! without them installed still get a green test run. macOS-only because
//! the production export backend (`FfmpegMacExporter`) is gated behind
//! `#[cfg(target_os = "macos")]`.
//!
//! What the test exercises end-to-end:
//!   1. Synthesise a deterministic source MP4 with ffmpeg's `testsrc` +
//!      `sine` filters so we know the exact source duration.
//!   2. Stage it as a finalised Segment under a real Course Folder.
//!   3. Write a transcript that maps onto the source seconds.
//!   4. Add EDL cuts that remove specific seconds.
//!   5. Run the production export pipeline (`ExportManager` +
//!      `FfmpegMacExporter`).
//!   6. ffprobe the output and confirm its duration matches the
//!      edl-adjusted target within tolerance (AC: "ffprobe the MP4 to
//!      confirm duration matches expected post-EDL length within
//!      tolerance").
//!   7. Parse the SRT and confirm its first cue's timecode aligns to the
//!      edited timeline, not the source.

#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use courseforge_lib::core::edits;
use courseforge_lib::core::transcript::{self, Transcript, Word};
use courseforge_lib::export_manager::{ExportManager, ExportStatus};
use courseforge_lib::exporter::ffmpeg_mac::FfmpegMacExporter;

fn have_binary(name: &str) -> bool {
    Command::new("/usr/bin/which")
        .arg(name)
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false)
}

fn synth_source_mp4(dst: &Path, duration_sec: u32) {
    // ffmpeg `testsrc` produces a deterministic colour pattern; pairing it
    // with `sine` gives us audio too, so the export filter's `aselect` path
    // is also exercised. yuv420p is required for QuickTime.
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel", "error",
            "-y",
            "-f", "lavfi",
            "-i", &format!("testsrc=duration={duration_sec}:size=320x240:rate=30"),
            "-f", "lavfi",
            "-i", &format!("sine=frequency=440:duration={duration_sec}"),
            "-c:v", "libx264",
            "-pix_fmt", "yuv420p",
            "-c:a", "aac",
            "-shortest",
            dst.to_str().unwrap(),
        ])
        .status()
        .expect("ffmpeg must be runnable for the synth source step");
    assert!(status.success(), "synthesising source MP4 failed");
}

fn probe_duration_sec(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_entries", "format=duration",
            "-of", "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe must be runnable");
    assert!(out.status.success(), "ffprobe failed: {out:?}");
    let s = String::from_utf8(out.stdout).unwrap().trim().to_string();
    s.parse::<f64>().unwrap()
}

fn course_with_staged_video(words: Vec<Word>) -> (tempfile::TempDir, PathBuf, String, f64) {
    let root = tempfile::tempdir().unwrap();
    let folder = courseforge_lib::core::course::create_course(root.path(), "Acme").unwrap();
    let m = courseforge_lib::core::course::add_module(&folder, "Intro").unwrap();
    let v = courseforge_lib::core::course::add_video(&folder, &m.id, "Welcome").unwrap();

    // Drop a synthesised MP4 directly into the Segment slot so we don't
    // have to drive the recorder for this test. The exporter only cares
    // that *something* playable lives at the expected path.
    let segs_dir = folder.join("videos").join(&v.id).join("segments");
    std::fs::create_dir_all(&segs_dir).unwrap();
    let seg_path = segs_dir.join("seg-1.mp4");
    synth_source_mp4(&seg_path, 10);
    let real_duration = probe_duration_sec(&seg_path);

    let t = Transcript::new(v.id.clone(), vec!["seg-1".into()], words);
    transcript::write_transcript(&folder, &v.id, &t).unwrap();

    (root, folder, v.id, real_duration)
}

#[test]
#[ignore = "requires ffmpeg + ffprobe on PATH; run with --ignored"]
fn record_cut_export_ffprobe_validates_edited_duration_and_srt_alignment() {
    if !have_binary("ffmpeg") || !have_binary("ffprobe") {
        eprintln!("skipping: ffmpeg/ffprobe not on PATH");
        return;
    }

    // Source clip is ~10 seconds. Cut 2.0–4.0 and 7.0–8.0 → expected edited
    // duration is 10 − 2 − 1 = 7 seconds.
    let words = vec![
        Word { start: 0.5, end: 1.5, text: "alpha".into() },
        Word { start: 2.5, end: 3.5, text: "removed".into() }, // inside cut #1
        Word { start: 5.0, end: 6.0, text: "beta".into() },
        Word { start: 7.5, end: 7.8, text: "gone".into() },    // inside cut #2
        Word { start: 9.0, end: 9.5, text: "gamma".into() },
    ];
    let (_root, folder, vid, source_duration) = course_with_staged_video(words);
    edits::append_cut(&folder, &vid, 2.0, 4.0).unwrap();
    edits::append_cut(&folder, &vid, 7.0, 8.0).unwrap();
    let expected_edited = source_duration - 3.0;

    let mgr = Arc::new(ExportManager::new(Box::new(FfmpegMacExporter::default())));
    let job = mgr.start_export(&folder, &vid, None).expect("start_export");
    let (mp4_path, srt_path) = match job.status {
        ExportStatus::Done { mp4, srt } => (mp4, srt),
        other => panic!("expected Done, got {other:?}"),
    };

    // 1. Final MP4 exists and plays — ffprobe just refuses to read it
    //    cleanly if the moov atom is missing, which is what would happen
    //    if ffmpeg got killed mid-mux.
    assert!(mp4_path.is_file());
    let edited_duration = probe_duration_sec(&mp4_path);

    // 2. Duration matches the EDL-adjusted target within a generous
    //    tolerance — re-encoding can shift the very end by a frame or
    //    two depending on the codec's GOP/keyframe alignment, so we
    //    allow ±0.5s here. The test is asserting "the cuts actually
    //    removed time", not "the duration is exact to the millisecond".
    let delta = (edited_duration - expected_edited).abs();
    assert!(
        delta < 0.5,
        "edited duration {edited_duration:.3}s differs from expected {expected_edited:.3}s by {delta:.3}s"
    );

    // 3. The sidecar SRT exists, parses, and uses the EDITED timeline. The
    //    first kept word was originally at 0.5s; with no earlier cuts
    //    that stays at 00:00:00,500. The second kept word ("beta") was at
    //    source 5.0s — after a 2s cut starting at 2.0s, it should land at
    //    edited 3.0s, i.e. 00:00:03,000. We assert the latter because it
    //    *only* matches if the SRT is timecoded against the edited
    //    timeline.
    let srt = std::fs::read_to_string(&srt_path).expect("srt should be readable");
    assert!(srt.contains("00:00:00,500"), "expected first cue at 0.500s, got:\n{srt}");
    assert!(srt.contains("00:00:03,000"), "expected remapped 'beta' at 3.000s, got:\n{srt}");
    // And the removed words are nowhere in the SRT.
    assert!(!srt.contains("removed"));
    assert!(!srt.contains("gone"));
}
