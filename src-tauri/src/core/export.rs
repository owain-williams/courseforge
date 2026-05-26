//! Export: turn a Video (its source Segment + EDL + transcript) into a
//! finished MP4 plus a sidecar `.srt`.
//!
//! This module is the *pure domain* side of export — derivations that are
//! testable without spawning ffmpeg:
//!
//! * [`keep_ranges`] — given a Segment duration and the EDL's cuts, return
//!   the ordered list of `(start, end)` ranges that should be **kept** in
//!   the output. Overlapping or touching cuts are merged so the renderer
//!   only ever sees non-overlapping ranges.
//! * [`edited_duration_sec`] — the total length of the rendered output.
//! * [`remap_to_edited`] — map a timestamp on the *source* timeline to its
//!   position on the *edited* timeline (or `None` if the moment is inside
//!   a cut and therefore not present in the output).
//! * [`srt_from_transcript`] — generate a SubRip caption file whose cue
//!   timecodes match the edited timeline (AC: `.srt` aligns with the edited
//!   video, not the source).
//! * [`default_export_dir`] — the convention for "where do exports go by
//!   default" (`<Course Folder>/exports/<video-id>/`). The user can override.
//!
//! Heavy lifting (running ffmpeg, progress, cancellation) lives in
//! [`crate::exporter`] behind a trait so this module stays IO-free and
//! exhaustively testable.

use std::path::{Path, PathBuf};

use crate::core::edits::Cut;
use crate::core::transcript::{Transcript, Word};

/// Cap on words-per-cue in the generated SRT. Tight enough that a single
/// caption fits comfortably on screen; loose enough that we don't flash a
/// new cue every word for fast speech.
const SRT_WORDS_PER_CUE: usize = 8;

/// Derive the list of *keep* ranges from a Segment's duration and the EDL's
/// cuts. Each returned range is `(start_sec, end_sec)` on the source
/// timeline. Output ranges are:
///
/// * sorted by start,
/// * non-overlapping (touching cuts are merged),
/// * clipped to `[0, source_duration_sec]`,
/// * and never zero-length (a fully-covered Segment returns `vec![]`).
pub fn keep_ranges(source_duration_sec: f64, cuts: &[Cut]) -> Vec<(f64, f64)> {
    if source_duration_sec <= 0.0 {
        return Vec::new();
    }
    let merged = merged_cut_intervals(cuts, source_duration_sec);

    let mut out = Vec::new();
    let mut cursor = 0.0_f64;
    for (start, end) in merged {
        if start > cursor {
            out.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < source_duration_sec {
        out.push((cursor, source_duration_sec));
    }
    out
}

/// Total length of the rendered output. Sum of keep-range widths.
pub fn edited_duration_sec(source_duration_sec: f64, cuts: &[Cut]) -> f64 {
    keep_ranges(source_duration_sec, cuts)
        .iter()
        .map(|(s, e)| e - s)
        .sum()
}

/// Map a source-timeline timestamp to its position on the edited timeline.
/// Returns `None` if the moment is inside a cut (and therefore doesn't
/// exist in the output).
///
/// Boundary rule: a cut interval is `[start, end)` — closed on the left,
/// open on the right. So `t == cut.start` lands *inside* the cut and a
/// `t == cut.end` is the first kept moment. See [`remap_end_to_edited`]
/// for the asymmetric variant used for word/range *end* timestamps.
pub fn remap_to_edited(t: f64, cuts: &[Cut]) -> Option<f64> {
    remap_with_intervals(t, &merged_cut_intervals(cuts, f64::INFINITY), false)
}

/// Like [`remap_to_edited`], but for *end* timestamps: a value lying exactly
/// on a cut's `start` is treated as kept (the word ends at the very moment
/// the cut begins; we don't want that to register as "inside").
///
/// This asymmetry matters whenever cut boundaries coincide with word
/// boundaries (which is the common case — the user clicks a word in the
/// transcript to set the cut, so the cut's edges are *defined* by word
/// boundaries).
pub fn remap_end_to_edited(t: f64, cuts: &[Cut]) -> Option<f64> {
    remap_with_intervals(t, &merged_cut_intervals(cuts, f64::INFINITY), true)
}

fn remap_with_intervals(t: f64, merged: &[(f64, f64)], end_inclusive: bool) -> Option<f64> {
    if t < 0.0 {
        return None;
    }
    let mut removed_before = 0.0_f64;
    for &(start, end) in merged {
        let before = if end_inclusive { t <= start } else { t < start };
        if before {
            return Some(t - removed_before);
        }
        if t < end {
            return None;
        }
        removed_before += end - start;
    }
    Some(t - removed_before)
}

/// Where exports land by default for a given Video. The user can override
/// at export time; this is just the suggested destination.
pub fn default_export_dir(course_folder: &Path, video_id: &str) -> PathBuf {
    course_folder.join("exports").join(video_id)
}

/// Generate a SubRip (`.srt`) caption file from the edited transcript.
/// Cues use *edited-timeline* timecodes (AC: `.srt` timecodes match the
/// edited timeline, not the source). Words that fall inside a cut are
/// dropped; words that span a cut boundary are dropped on the cut side
/// only (the word's start/end is intersected with each keep range).
///
/// Cues are formed by grouping consecutive kept words up to
/// [`SRT_WORDS_PER_CUE`]; a cue is also broken whenever the gap between two
/// adjacent words in the edited timeline is non-zero (i.e. a cut sits
/// between them).
pub fn srt_from_transcript(transcript: &Transcript, cuts: &[Cut]) -> String {
    let kept = kept_words_remapped(&transcript.words, cuts);
    let mut out = String::new();
    let mut cue_index = 1usize;
    for cue in group_into_cues(&kept) {
        let first = cue.first().expect("cue is non-empty by construction");
        let last = cue.last().expect("cue is non-empty by construction");
        let text: String = cue.iter().map(|w| w.text.as_str()).collect();
        let text = text.trim().to_string();
        if text.is_empty() {
            continue;
        }
        out.push_str(&format!("{cue_index}\n"));
        out.push_str(&format!(
            "{} --> {}\n",
            format_timecode(first.start),
            format_timecode(last.end),
        ));
        out.push_str(&text);
        out.push_str("\n\n");
        cue_index += 1;
    }
    out
}

// --- helpers (pub(crate) for export tests) -----------------------------------

/// Normalise the EDL's cuts: sort by start, drop zero-/negative-width cuts,
/// merge overlapping or touching cuts, and clip every cut to
/// `[0, max_duration]`. The merged intervals are what `keep_ranges` and
/// `remap_to_edited` actually work against.
fn merged_cut_intervals(cuts: &[Cut], max_duration: f64) -> Vec<(f64, f64)> {
    let mut intervals: Vec<(f64, f64)> = cuts
        .iter()
        .filter_map(|c| {
            let s = c.start_sec.max(0.0);
            let e = c.end_sec.min(max_duration);
            (e > s).then_some((s, e))
        })
        .collect();
    intervals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut merged: Vec<(f64, f64)> = Vec::new();
    for (s, e) in intervals {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

fn kept_words_remapped(words: &[Word], cuts: &[Cut]) -> Vec<Word> {
    let merged = merged_cut_intervals(cuts, f64::INFINITY);
    words
        .iter()
        .filter_map(|w| {
            // A word that straddles a cut boundary will have a remapped
            // start but no remapped end (or vice versa). Skip these — the
            // visual effect of a word being cut in half mid-utterance is
            // worse than dropping it. The `end_inclusive` asymmetry on the
            // end side lets a word ending exactly at a cut's start
            // (the normal case) stay kept rather than getting dropped.
            let start = remap_with_intervals(w.start, &merged, false)?;
            let end = remap_with_intervals(w.end, &merged, true)?;
            if end <= start {
                return None;
            }
            Some(Word {
                start,
                end,
                text: w.text.clone(),
            })
        })
        .collect()
}

fn group_into_cues(words: &[Word]) -> Vec<Vec<Word>> {
    let mut out: Vec<Vec<Word>> = Vec::new();
    let mut cur: Vec<Word> = Vec::new();
    let mut last_end = 0.0_f64;
    for w in words {
        let break_here = !cur.is_empty()
            && (cur.len() >= SRT_WORDS_PER_CUE || (w.start - last_end).abs() > 1e-6);
        if break_here {
            out.push(std::mem::take(&mut cur));
        }
        last_end = w.end;
        cur.push(w.clone());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn format_timecode(t: f64) -> String {
    let total_ms = (t * 1000.0).round().max(0.0) as u64;
    let h = total_ms / 3_600_000;
    let m = (total_ms / 60_000) % 60;
    let s = (total_ms / 1_000) % 60;
    let ms = total_ms % 1_000;
    format!("{h:02}:{m:02}:{s:02},{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::transcript::Transcript;

    fn cut(id: &str, start: f64, end: f64) -> Cut {
        Cut {
            id: id.into(),
            start_sec: start,
            end_sec: end,
        }
    }

    fn word(start: f64, end: f64, text: &str) -> Word {
        Word { start, end, text: text.into() }
    }

    // -- keep_ranges ----------------------------------------------------------

    #[test]
    fn keep_ranges_no_cuts_is_one_range_spanning_the_whole_segment() {
        assert_eq!(keep_ranges(10.0, &[]), vec![(0.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_single_middle_cut_splits_into_two_ranges() {
        let ranges = keep_ranges(10.0, &[cut("a", 3.0, 5.0)]);
        assert_eq!(ranges, vec![(0.0, 3.0), (5.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_cut_at_start_drops_leading_range() {
        let ranges = keep_ranges(10.0, &[cut("a", 0.0, 2.0)]);
        assert_eq!(ranges, vec![(2.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_cut_at_end_drops_trailing_range() {
        let ranges = keep_ranges(10.0, &[cut("a", 8.0, 10.0)]);
        assert_eq!(ranges, vec![(0.0, 8.0)]);
    }

    #[test]
    fn keep_ranges_overlapping_cuts_are_merged_into_a_single_gap() {
        let ranges = keep_ranges(
            10.0,
            &[cut("a", 2.0, 4.0), cut("b", 3.5, 6.0)],
        );
        assert_eq!(ranges, vec![(0.0, 2.0), (6.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_touching_cuts_are_merged() {
        // Two cuts that share a boundary (4.0) should merge — otherwise the
        // exporter would render a zero-width "kept" sliver, which most
        // muxers refuse.
        let ranges = keep_ranges(
            10.0,
            &[cut("a", 2.0, 4.0), cut("b", 4.0, 6.0)],
        );
        assert_eq!(ranges, vec![(0.0, 2.0), (6.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_cuts_arriving_out_of_order_are_sorted() {
        let ranges = keep_ranges(
            10.0,
            &[cut("late", 7.0, 8.0), cut("early", 1.0, 2.0)],
        );
        assert_eq!(ranges, vec![(0.0, 1.0), (2.0, 7.0), (8.0, 10.0)]);
    }

    #[test]
    fn keep_ranges_cut_covering_whole_segment_returns_empty() {
        let ranges = keep_ranges(5.0, &[cut("a", 0.0, 5.0)]);
        assert!(ranges.is_empty());
    }

    #[test]
    fn keep_ranges_cut_extending_past_segment_end_is_clipped() {
        // The user can construct a cut whose end is beyond the source
        // duration (transcript over-runs, recorder time mismatch). The
        // exporter must never be handed a range past the source end.
        let ranges = keep_ranges(5.0, &[cut("a", 3.0, 99.0)]);
        assert_eq!(ranges, vec![(0.0, 3.0)]);
    }

    // -- edited_duration_sec ---------------------------------------------------

    #[test]
    fn edited_duration_equals_source_when_no_cuts() {
        assert_eq!(edited_duration_sec(10.0, &[]), 10.0);
    }

    #[test]
    fn edited_duration_subtracts_each_cut_width() {
        let d = edited_duration_sec(10.0, &[cut("a", 1.0, 2.5), cut("b", 5.0, 6.0)]);
        // 10 − 1.5 − 1.0 = 7.5
        assert!((d - 7.5).abs() < 1e-9);
    }

    // -- remap_to_edited -------------------------------------------------------

    #[test]
    fn remap_before_any_cut_is_identity() {
        let t = remap_to_edited(0.5, &[cut("a", 1.0, 2.0)]).unwrap();
        assert!((t - 0.5).abs() < 1e-9);
    }

    #[test]
    fn remap_inside_a_cut_returns_none() {
        assert!(remap_to_edited(1.5, &[cut("a", 1.0, 2.0)]).is_none());
    }

    #[test]
    fn remap_after_a_cut_subtracts_the_cuts_width() {
        // Source time 4.0, with a 1s cut earlier (1.0-2.0): edited time is 3.0.
        let t = remap_to_edited(4.0, &[cut("a", 1.0, 2.0)]).unwrap();
        assert!((t - 3.0).abs() < 1e-9);
    }

    #[test]
    fn remap_with_multiple_cuts_subtracts_cumulative_width() {
        let cuts = vec![cut("a", 1.0, 2.0), cut("b", 4.0, 5.0)];
        let t = remap_to_edited(6.0, &cuts).unwrap();
        assert!((t - 4.0).abs() < 1e-9);
    }

    #[test]
    fn remap_at_cut_start_is_inside_the_cut_for_normal_points() {
        // Start-style points use a half-open `[start, end)` interval, so a
        // moment that lands exactly at cut.start is *inside* the cut.
        // (This is what we want for word START timestamps — a word that
        // begins exactly where a cut begins really is the first removed
        // word.)
        assert!(remap_to_edited(1.0, &[cut("a", 1.0, 2.0)]).is_none());
    }

    #[test]
    fn remap_end_at_cut_start_is_kept_so_word_ends_dont_get_swallowed() {
        // End-style points use the opposite convention: cut.start is the
        // first removed moment, so a word ending *exactly* there finishes
        // just before the cut and stays kept. Without this asymmetry the
        // SRT would lose every word the user touches when they cut by
        // word-boundary (which is the common case).
        let t = remap_end_to_edited(1.0, &[cut("a", 1.0, 2.0)]).unwrap();
        assert!((t - 1.0).abs() < 1e-9);
    }

    // -- default_export_dir ---------------------------------------------------

    #[test]
    fn default_export_dir_lives_under_course_folder_exports_video_id() {
        let folder = PathBuf::from("/tmp/course");
        let dir = default_export_dir(&folder, "vid-1");
        assert_eq!(dir, PathBuf::from("/tmp/course/exports/vid-1"));
    }

    // -- srt generation -------------------------------------------------------

    #[test]
    fn srt_for_no_cuts_emits_one_cue_per_words_chunk_with_source_timecodes() {
        let t = Transcript::new(
            "v".into(),
            vec![],
            vec![
                word(0.0, 0.5, "Hello"),
                word(0.5, 1.0, " world"),
            ],
        );
        let srt = srt_from_transcript(&t, &[]);
        assert!(srt.starts_with("1\n00:00:00,000 --> 00:00:01,000\n"));
        assert!(srt.contains("Hello world"));
    }

    #[test]
    fn srt_drops_words_inside_cuts_and_remaps_remaining_timecodes() {
        // Cut "world" out. The remaining words should land on the edited
        // timeline — "Hello" stays where it started, "again" moves earlier
        // by the cut's width.
        let t = Transcript::new(
            "v".into(),
            vec![],
            vec![
                word(0.0, 0.5, "Hello"),
                word(0.5, 1.0, "world"),
                word(2.0, 2.5, "again"),
            ],
        );
        let cuts = vec![cut("a", 0.5, 2.0)]; // removes "world"
        let srt = srt_from_transcript(&t, &cuts);

        // "world" must not appear.
        assert!(!srt.contains("world"));
        // "Hello" stays at 0.0–0.5; "again" originally at 2.0–2.5 lands at
        // edited 0.5–1.0 (cut width 1.5s). On the edited timeline the two
        // are contiguous (the cut's source-timeline gap collapses to 0),
        // so they share one cue with timecodes 0.000 → 1.000.
        assert!(srt.contains("00:00:00,000 --> 00:00:01,000"), "got:\n{srt}");
        assert!(srt.contains("Hello"));
        assert!(srt.contains("again"));
    }

    #[test]
    fn srt_drops_every_word_that_falls_inside_a_cut() {
        // Words from before and after the cut should all appear; words
        // inside the cut never do. This is the AC: "`.srt` aligns with
        // the edited timeline" — anything cut never reaches the captions.
        let t = Transcript::new(
            "v".into(),
            vec![],
            vec![
                word(0.0, 0.5, "alpha"),
                word(0.5, 1.0, "beta"),
                word(1.5, 2.0, "MID-CUT"),
                word(2.0, 2.5, "ALSO-CUT"),
                word(3.0, 3.5, "gamma"),
                word(3.5, 4.0, "delta"),
            ],
        );
        let cuts = vec![cut("a", 1.0, 3.0)];
        let srt = srt_from_transcript(&t, &cuts);

        assert!(srt.contains("alpha"));
        assert!(srt.contains("beta"));
        assert!(srt.contains("gamma"));
        assert!(srt.contains("delta"));
        assert!(!srt.contains("MID-CUT"));
        assert!(!srt.contains("ALSO-CUT"));
    }

    #[test]
    fn srt_timecodes_are_zero_padded_hh_mm_ss_comma_mmm() {
        let t = Transcript::new(
            "v".into(),
            vec![],
            vec![word(0.0, 65.123, "one-long-word")],
        );
        let srt = srt_from_transcript(&t, &[]);
        assert!(srt.contains("00:00:00,000 --> 00:01:05,123"), "got: {srt}");
    }

    #[test]
    fn srt_for_a_transcript_with_no_kept_words_is_empty() {
        let t = Transcript::new("v".into(), vec![], vec![word(0.0, 1.0, "byebye")]);
        let cuts = vec![cut("a", 0.0, 1.0)];
        assert!(srt_from_transcript(&t, &cuts).is_empty());
    }
}
