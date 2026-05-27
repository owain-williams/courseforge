//! Segments are the raw recorded captures that belong to a Video.
//!
//! On disk, per ADR-0001 and CONTEXT.md, downstream per-Video state lives in
//! per-Video subfolders rather than inside `course.json`. Segments follow that
//! rule: they sit at `videos/<video-id>/segments/<segment-id>.mov`, and the
//! Library / Course view discovers them by scanning the folder — there is no
//! segments[] array in `course.json` that could drift out of sync.
//!
//! Per ADR-0002 (Phase 1), capture writes a `.partial.mov` directly via
//! AVAssetWriter; on Keep we rename the partial to its final name and emit a
//! per-Segment sidecar JSON (`<segment-id>.json`) carrying take grouping,
//! source role, device, defaults, and the reason the recording ended.
//! Discard removes the partial directly.
//!
//! v1 Course Folders predate this change. We continue to discover legacy
//! `.mp4` finals and legacy `.partial.mkv` orphans on read so existing
//! Courses still open. Importing a v1 `.partial.mkv` orphan goes through a
//! small inline ffmpeg remux (the v1 behaviour kept on the orphan path
//! only); Phase 7 polishes this.

use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::capture::{CaptureRequest, SegmentSidecar, SourceRole};
use crate::core::error::{CoreError, Result};

/// New (Phase 1) extension for video-source finalised Segments.
pub const SEGMENT_EXT: &str = "mov";
/// New (Phase 1) partial suffix for in-progress video-source captures.
pub const PARTIAL_SUFFIX: &str = "partial.mov";
/// Phase 2 audio-only finalised-Segment extension (microphone, system
/// audio). AAC-in-MP4 container — same shape AVAssetWriter writes when
/// given only an audio input.
pub const SEGMENT_EXT_AUDIO: &str = "m4a";
/// Phase 2 audio-only partial suffix.
pub const PARTIAL_SUFFIX_AUDIO: &str = "partial.m4a";
/// Sidecar JSON extension, sibling to the Segment file.
pub const SIDECAR_EXT: &str = "json";

/// v1 finalised-Segment extension. Kept readable forever — v1 Course Folders
/// open under v2 builds without rewriting on disk.
pub const LEGACY_SEGMENT_EXT: &str = "mp4";
/// v1 partial-Segment suffix. Kept discoverable so orphans from before
/// Phase 1 still surface in `scan_orphans` and can be adopted (the adopt
/// path runs ffmpeg under the hood for these — the only place ffmpeg
/// touches the recorder side post-Phase-1).
pub const LEGACY_PARTIAL_SUFFIX: &str = "partial.mkv";

/// Closed-taxonomy mapping from Source Role to the partial/final file
/// extensions the recorder backend writes. Audio-only sources land in
/// `.m4a` (AAC); video-and-audio sources land in `.mov` (QuickTime).
pub fn partial_suffix_for(role: SourceRole) -> &'static str {
    if role.is_audio_only() {
        PARTIAL_SUFFIX_AUDIO
    } else {
        PARTIAL_SUFFIX
    }
}

pub fn segment_ext_for(role: SourceRole) -> &'static str {
    if role.is_audio_only() {
        SEGMENT_EXT_AUDIO
    } else {
        SEGMENT_EXT
    }
}

/// A finalised Segment on disk. Discovered by scanning, not stored in course.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Segment {
    pub id: String,
    #[serde(rename = "videoId")]
    pub video_id: String,
    /// Path relative to the Course Folder (e.g. `videos/<vid>/segments/<sid>.mov`).
    /// Relative so a Course Folder copied to another Mac still resolves.
    pub path: PathBuf,
}

/// An in-progress segment file that survived a crash. `path` points at the
/// surviving `.partial.mov` (or legacy `.partial.mkv`) so the UI / caller
/// knows to offer import-or-discard rather than treating it as a finished take.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OrphanSegment {
    pub id: String,
    #[serde(rename = "videoId")]
    pub video_id: String,
    pub path: PathBuf,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn segments_dir(folder: &Path, video_id: &str) -> PathBuf {
    folder.join("videos").join(video_id).join("segments")
}

fn sidecar_path_for(folder: &Path, video_id: &str, segment_id: &str) -> PathBuf {
    segments_dir(folder, video_id).join(format!("{segment_id}.{SIDECAR_EXT}"))
}

/// Allocate a fresh Segment id and the absolute path the recorder should
/// stream the `.partial.mov` to. The segments folder is created if missing.
/// Returns the absolute path so the recorder doesn't have to know about the
/// Course Folder layout — callers serialise it back to a relative path
/// before persisting (see `to_relative`).
///
/// Backwards-compatible helper for the single-source Phase 1 call sites
/// that still exist. Phase 2 multi-source flows use
/// [`prepare_take_paths`].
pub fn prepare_segment_path(folder: &Path, video_id: &str) -> Result<(String, PathBuf)> {
    let dir = segments_dir(folder, video_id);
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let id = new_id();
    let path = dir.join(format!("{id}.{PARTIAL_SUFFIX}"));
    Ok((id, path))
}

/// Per-source allocation in a Take — one of these per `CaptureRequest`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeSlot {
    pub segment_id: String,
    pub partial_path: PathBuf,
}

/// File extension for the per-Take in-progress marker (issue #38). One of
/// these lands at `videos/<vid>/segments/<takeId>.in-progress.json` at
/// Start; clean Stop removes it. A crashed app leaves it behind so the
/// next-launch scanner can group partial files by takeId.
pub const TAKE_MARKER_SUFFIX: &str = "in-progress.json";

/// One source row inside a Take marker. Mirrors the runtime
/// `CaptureRequest` + the segment id the manager allocated, so a recovered
/// Take can rebuild per-Segment sidecars without consulting the in-memory
/// `RecordingSession` (which dies with the app).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeMarkerSource {
    pub segment_id: String,
    pub request: CaptureRequest,
}

/// On-disk marker for one in-progress Take. The scanner reads these to
/// group surviving partials by `takeId`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeMarker {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub take_id: String,
    pub video_id: String,
    /// ISO-8601 UTC timestamp the Take started (same value every
    /// per-Segment sidecar inherits on Keep).
    pub recorded_at: String,
    /// Optional Scene id that drove the Take. Phase 2 makes this purely
    /// informational so the orphan-recovery UI can render a richer label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,
    pub sources: Vec<TakeMarkerSource>,
}

impl TakeMarker {
    pub const SCHEMA_VERSION: u32 = 1;
}

fn take_marker_path(folder: &Path, video_id: &str, take_id: &str) -> PathBuf {
    segments_dir(folder, video_id).join(format!("{take_id}.{TAKE_MARKER_SUFFIX}"))
}

/// Atomic write of a Take marker to `<segments-dir>/<takeId>.in-progress.json`.
pub fn write_take_marker(folder: &Path, video_id: &str, marker: &TakeMarker) -> Result<()> {
    let path = take_marker_path(folder, video_id, &marker.take_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(marker)
        .expect("TakeMarker serialises cleanly — all fields are JSON-safe");
    std::fs::write(&tmp, bytes).map_err(|e| CoreError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    Ok(())
}

/// Remove a Take marker. Idempotent — missing files are not an error.
pub fn remove_take_marker(folder: &Path, video_id: &str, take_id: &str) -> Result<()> {
    let path = take_marker_path(folder, video_id, take_id);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(CoreError::Io { path, source: e }),
    }
}

/// Read a Take marker by id. Returns `Ok(None)` if the file is missing.
pub fn read_take_marker(
    folder: &Path,
    video_id: &str,
    take_id: &str,
) -> Result<Option<TakeMarker>> {
    let path = take_marker_path(folder, video_id, take_id);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(CoreError::Io { path, source: e }),
    };
    let marker: TakeMarker = serde_json::from_slice(&bytes).map_err(|source| {
        CoreError::InvalidTranscriptJson {
            path: path.clone(),
            source,
        }
    })?;
    Ok(Some(marker))
}

/// One per-Take row surfaced by [`scan_orphan_takes`]. Used by the orphan
/// recovery UI: one row per crashed Take, with Import / Discard buttons
/// that act on every Segment in the Take at once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OrphanTake {
    pub take_id: String,
    pub video_id: String,
    /// `recorded_at` from the marker, or `None` for v1 legacy orphans
    /// that predate Take markers.
    pub recorded_at: Option<String>,
    pub scene_id: Option<String>,
    pub segments: Vec<OrphanTakeSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OrphanTakeSegment {
    pub segment_id: String,
    /// Source Role from the Take marker / sidecar. v1 legacy orphans
    /// that predate the marker fall back to `Screen` (they were always
    /// single-source screen recordings under the v1 ffmpeg pipeline).
    pub source_role: SourceRole,
    /// Partial path on disk, relative to the Course Folder. Same shape
    /// `OrphanSegment` already returns.
    pub partial_path: PathBuf,
}

/// Allocate N segment ids and partial paths for one Take, picking the
/// right partial extension per Source Role (`.partial.mov` for video,
/// `.partial.m4a` for audio-only). The segments folder is created once
/// up front rather than per slot.
pub fn prepare_take_paths(
    folder: &Path,
    video_id: &str,
    requests: &[CaptureRequest],
) -> Result<Vec<TakeSlot>> {
    let dir = segments_dir(folder, video_id);
    std::fs::create_dir_all(&dir).map_err(|e| CoreError::Io {
        path: dir.clone(),
        source: e,
    })?;
    let mut out = Vec::with_capacity(requests.len());
    for req in requests {
        let id = new_id();
        let suffix = partial_suffix_for(req.role);
        let path = dir.join(format!("{id}.{suffix}"));
        out.push(TakeSlot { segment_id: id, partial_path: path });
    }
    Ok(out)
}

/// Promote an in-progress `.partial.mov` to its final name and write the
/// per-Segment sidecar JSON next to it. Idempotent: if the partial is gone
/// but the final `.mov` is already in place, that's "already finalised" and
/// we just (re)write the sidecar so callers can update its metadata after
/// the fact.
///
/// No remux. AVAssetWriter writes `.mov` directly; WebKit `<video>` plays
/// `.mov` natively (the v1 `.partial.mkv → .mp4` remux retires here).
pub fn finalize_segment(
    folder: &Path,
    video_id: &str,
    segment_id: &str,
    sidecar: SegmentSidecar,
) -> Result<Segment> {
    let dir = segments_dir(folder, video_id);
    let partial_suffix = partial_suffix_for(sidecar.source_role);
    let final_ext = segment_ext_for(sidecar.source_role);
    let partial = dir.join(format!("{segment_id}.{partial_suffix}"));
    let final_path = dir.join(format!("{segment_id}.{final_ext}"));

    if partial.is_file() {
        // Rename is atomic on the same filesystem (segments live under one
        // course folder, so this holds). If a stale final exists from a
        // half-completed previous finalize, replace it.
        if final_path.exists() {
            let _ = std::fs::remove_file(&final_path);
        }
        std::fs::rename(&partial, &final_path).map_err(|e| CoreError::Io {
            path: final_path.clone(),
            source: e,
        })?;
    } else if !final_path.is_file() {
        return Err(CoreError::SegmentNotFound(segment_id.to_string()));
    }

    write_sidecar(folder, video_id, segment_id, &sidecar)?;

    let rel = to_relative(folder, &final_path);
    Ok(Segment {
        id: segment_id.to_string(),
        video_id: video_id.to_string(),
        path: rel,
    })
}

/// Adopt a crash-recovered partial as a finished Segment. Two shapes:
///
/// * `.partial.mov` (Phase 1+ orphan): rename to `.mov`, write the sidecar
///   the caller supplies (with `endedReason: crashed`).
/// * `.partial.mkv` (v1 legacy orphan): remux to `.mp4` via `legacy_remux_fn`
///   so the result plays in WebKit. No sidecar is written for v1 orphans —
///   v1 never had them. Phase 7 will harmonise this.
///
/// The split is invisible to callers: pass the take-grouping / device info
/// for new-shape orphans and a closure that knows how to remux for legacy.
pub fn adopt_orphan(
    folder: &Path,
    video_id: &str,
    segment_id: &str,
    new_shape_sidecar: SegmentSidecar,
    legacy_remux_fn: impl FnOnce(&Path, &Path) -> Result<()>,
) -> Result<Segment> {
    let dir = segments_dir(folder, video_id);
    let video_partial = dir.join(format!("{segment_id}.{PARTIAL_SUFFIX}"));
    let audio_partial = dir.join(format!("{segment_id}.{PARTIAL_SUFFIX_AUDIO}"));
    let legacy_partial = dir.join(format!("{segment_id}.{LEGACY_PARTIAL_SUFFIX}"));

    if video_partial.is_file() || audio_partial.is_file() {
        // Phase 1+ path — same as a normal finalize but with crashed reason
        // already baked into the caller-supplied sidecar. finalize_segment
        // picks the right partial suffix based on the sidecar's source role.
        return finalize_segment(folder, video_id, segment_id, new_shape_sidecar);
    }

    if legacy_partial.is_file() {
        let legacy_final = dir.join(format!("{segment_id}.{LEGACY_SEGMENT_EXT}"));
        legacy_remux_fn(&legacy_partial, &legacy_final)?;
        // Drop the partial; if removal fails it's not fatal — the next
        // scan_orphans call will short-circuit on the existing .mp4.
        let _ = std::fs::remove_file(&legacy_partial);
        return Ok(Segment {
            id: segment_id.to_string(),
            video_id: video_id.to_string(),
            path: to_relative(folder, &legacy_final),
        });
    }

    // Already-finalised by a previous adopt attempt? Any extension counts.
    for ext in [SEGMENT_EXT, SEGMENT_EXT_AUDIO, LEGACY_SEGMENT_EXT] {
        let candidate = dir.join(format!("{segment_id}.{ext}"));
        if candidate.is_file() {
            return Ok(Segment {
                id: segment_id.to_string(),
                video_id: video_id.to_string(),
                path: to_relative(folder, &candidate),
            });
        }
    }

    Err(CoreError::SegmentNotFound(segment_id.to_string()))
}

/// Delete the in-progress partial for a Segment. Handles both the new
/// `.partial.mov` and the legacy `.partial.mkv`. Missing files are treated
/// as success — Discard is meant to be safe to call after a crash or after
/// the user already cleaned up by hand.
pub fn discard_partial(folder: &Path, video_id: &str, segment_id: &str) -> Result<()> {
    let dir = segments_dir(folder, video_id);
    for suffix in [PARTIAL_SUFFIX, PARTIAL_SUFFIX_AUDIO, LEGACY_PARTIAL_SUFFIX] {
        let partial = dir.join(format!("{segment_id}.{suffix}"));
        if partial.is_file() {
            std::fs::remove_file(&partial).map_err(|e| CoreError::Io {
                path: partial,
                source: e,
            })?;
        }
    }
    Ok(())
}

/// Write (or overwrite) the sidecar JSON for a Segment. Atomic in the
/// usual write-temp-then-rename sense so a crashed write can't leave a
/// half-finished JSON on disk.
pub fn write_sidecar(
    folder: &Path,
    video_id: &str,
    segment_id: &str,
    sidecar: &SegmentSidecar,
) -> Result<()> {
    let path = sidecar_path_for(folder, video_id, segment_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(sidecar)
        .expect("SegmentSidecar serialises cleanly — all fields are JSON-safe");
    std::fs::write(&tmp, bytes).map_err(|e| CoreError::Io {
        path: tmp.clone(),
        source: e,
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| CoreError::Io {
        path: path.clone(),
        source: e,
    })?;
    Ok(())
}

/// Read the sidecar JSON for a Segment, if present. Returns `None` for
/// Segments without a sidecar (e.g. legacy v1 `.mp4` Segments that predate
/// this change) so callers can fall back to whatever defaults they want.
pub fn read_sidecar(
    folder: &Path,
    video_id: &str,
    segment_id: &str,
) -> Result<Option<SegmentSidecar>> {
    let path = sidecar_path_for(folder, video_id, segment_id);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(CoreError::Io { path, source: e }),
    };
    let sidecar: SegmentSidecar = serde_json::from_slice(&bytes)
        .map_err(|source| CoreError::InvalidTranscriptJson {
            // Reusing the transcript variant intentionally — sidecars predate
            // their own error variant and the surface is the same shape
            // (path + serde_json::Error). Worth its own variant if a future
            // pass adds more sidecar-specific diagnostics.
            path: path.clone(),
            source,
        })?;
    Ok(Some(sidecar))
}

/// List finalised Segments for a Video by scanning its segments folder.
/// Hidden files, partials, and the sidecar JSONs are skipped. Both the new
/// `.mov` and the legacy `.mp4` extension are picked up so v1 Course
/// Folders still play. Order is by id (filename stem) so the result is
/// stable for tests and for UI rendering.
pub fn list_segments(folder: &Path, video_id: &str) -> Result<Vec<Segment>> {
    let dir = segments_dir(folder, video_id);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(|e| CoreError::Io {
        path: dir.clone(),
        source: e,
    })? {
        let entry = entry.map_err(|e| CoreError::Io {
            path: dir.clone(),
            source: e,
        })?;
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        // Skip every partial-suffix shape and the sidecar JSON files.
        if name.ends_with(&format!(".{PARTIAL_SUFFIX}"))
            || name.ends_with(&format!(".{PARTIAL_SUFFIX_AUDIO}"))
            || name.ends_with(&format!(".{LEGACY_PARTIAL_SUFFIX}"))
            || name.ends_with(&format!(".{SIDECAR_EXT}"))
        {
            continue;
        }
        let id = strip_segment_ext(name);
        let id = match id {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        out.push(Segment {
            id,
            video_id: video_id.to_string(),
            path: to_relative(folder, &path),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// Group surviving partial files under any Video's segments folder into
/// per-Take rows (issue #38). For every Take marker on disk we list its
/// expected segments and the partials that landed; any leftover partial
/// that doesn't appear in a marker — including v1 `.partial.mkv` orphans
/// that predate Take markers — surfaces as a single-Segment Take so the
/// UI flow stays uniform.
pub fn scan_orphan_takes(folder: &Path) -> Result<Vec<OrphanTake>> {
    let videos_root = folder.join("videos");
    if !videos_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out: Vec<OrphanTake> = Vec::new();
    // Track partials we've assigned to a Take so we don't surface them
    // twice as a synthetic single-segment Take.
    let mut claimed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for video_entry in std::fs::read_dir(&videos_root).map_err(|e| CoreError::Io {
        path: videos_root.clone(),
        source: e,
    })? {
        let video_entry = match video_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !video_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let video_id = match video_entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let segs = video_entry.path().join("segments");
        if !segs.is_dir() {
            continue;
        }

        // First pass: find every Take marker and read it. Each marker is
        // one row in the output. Any partial whose segment id matches a
        // marker's source list is "claimed" by that Take row.
        let entries: Vec<_> = std::fs::read_dir(&segs)
            .map_err(|e| CoreError::Io {
                path: segs.clone(),
                source: e,
            })?
            .filter_map(|e| e.ok())
            .collect();

        for entry in &entries {
            let name = match entry.file_name().to_str().map(str::to_string) {
                Some(s) => s,
                None => continue,
            };
            let suffix = format!(".{TAKE_MARKER_SUFFIX}");
            if !name.ends_with(&suffix) {
                continue;
            }
            let take_id = name.strip_suffix(&suffix).unwrap_or(&name).to_string();
            let marker = match read_take_marker(folder, &video_id, &take_id)? {
                Some(m) => m,
                None => continue,
            };

            let mut segments_out = Vec::with_capacity(marker.sources.len());
            for src in &marker.sources {
                let suffix = partial_suffix_for(src.request.role);
                let partial = segs.join(format!("{}.{}", src.segment_id, suffix));
                if partial.is_file() {
                    claimed.insert(partial.clone());
                    segments_out.push(OrphanTakeSegment {
                        segment_id: src.segment_id.clone(),
                        source_role: src.request.role,
                        partial_path: to_relative(folder, &partial),
                    });
                }
            }
            out.push(OrphanTake {
                take_id: marker.take_id,
                video_id: video_id.clone(),
                recorded_at: Some(marker.recorded_at),
                scene_id: marker.scene_id,
                segments: segments_out,
            });
        }

        // Second pass: any partial not yet claimed by a Take marker
        // becomes a single-Segment synthetic Take. Sidecars-on-disk
        // would normally come from previous Keeps and shouldn't be
        // here, so this branch is the v1 `.partial.mkv` / pre-#38
        // `.partial.mov` / `.partial.m4a` recovery path.
        for entry in &entries {
            let path = entry.path();
            if claimed.contains(&path) {
                continue;
            }
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            let id = match strip_partial_suffix(name) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            // Sidecar may still exist on disk from a partial-write of a
            // previous Keep; read it if so to recover the role + device.
            let sidecar = read_sidecar(folder, &video_id, &id)?;
            let source_role = sidecar.as_ref().map(|s| s.source_role).unwrap_or(SourceRole::Screen);
            let take_id = sidecar
                .as_ref()
                .map(|s| s.take_id.clone())
                .unwrap_or_else(|| format!("legacy-{id}"));
            let recorded_at = sidecar.as_ref().map(|s| s.recorded_at.clone());
            out.push(OrphanTake {
                take_id,
                video_id: video_id.clone(),
                recorded_at,
                scene_id: None,
                segments: vec![OrphanTakeSegment {
                    segment_id: id,
                    source_role,
                    partial_path: to_relative(folder, &path),
                }],
            });
        }
    }
    out.sort_by(|a, b| (a.video_id.as_str(), a.take_id.as_str()).cmp(&(&b.video_id, &b.take_id)));
    Ok(out)
}

/// Find every partial file under any Video's segments folder — both the new
/// `.partial.mov` shape and the legacy `.partial.mkv`. Called on Course
/// window open so the user can be offered the chance to import or discard
/// captures that didn't survive a clean Stop.
pub fn scan_orphans(folder: &Path) -> Result<Vec<OrphanSegment>> {
    let videos_root = folder.join("videos");
    if !videos_root.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for video_entry in std::fs::read_dir(&videos_root).map_err(|e| CoreError::Io {
        path: videos_root.clone(),
        source: e,
    })? {
        let video_entry = match video_entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !video_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let video_id = match video_entry.file_name().to_str() {
            Some(s) => s.to_string(),
            None => continue,
        };
        let segs = video_entry.path().join("segments");
        if !segs.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&segs).map_err(|e| CoreError::Io {
            path: segs.clone(),
            source: e,
        })? {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            let path = entry.path();
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            let id = strip_partial_suffix(name);
            let id = match id {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            out.push(OrphanSegment {
                id,
                video_id: video_id.clone(),
                path: to_relative(folder, &path),
            });
        }
    }
    out.sort_by(|a, b| (a.video_id.as_str(), a.id.as_str()).cmp(&(&b.video_id, &b.id)));
    Ok(out)
}

fn strip_segment_ext(name: &str) -> Option<&str> {
    name.strip_suffix(&format!(".{SEGMENT_EXT}"))
        .or_else(|| name.strip_suffix(&format!(".{SEGMENT_EXT_AUDIO}")))
        .or_else(|| name.strip_suffix(&format!(".{LEGACY_SEGMENT_EXT}")))
}

fn strip_partial_suffix(name: &str) -> Option<&str> {
    name.strip_suffix(&format!(".{PARTIAL_SUFFIX}"))
        .or_else(|| name.strip_suffix(&format!(".{PARTIAL_SUFFIX_AUDIO}")))
        .or_else(|| name.strip_suffix(&format!(".{LEGACY_PARTIAL_SUFFIX}")))
}

fn to_relative(folder: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(folder).map(PathBuf::from).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::capture::{
        CompositionDefaults, Device, EndedReason, SegmentSidecar, SourceRole,
    };

    fn course_folder() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn dummy_request(role: SourceRole) -> CaptureRequest {
        CaptureRequest {
            role,
            device: Device {
                id: "default".into(),
                label: "Default".into(),
            },
            defaults: CompositionDefaults::default(),
            is_transcript_source: false,
        }
    }

    fn sample_sidecar() -> SegmentSidecar {
        SegmentSidecar::new(
            "take-1",
            SourceRole::Screen,
            Device {
                id: "default".into(),
                label: "Main Display".into(),
            },
            "2026-05-26T12:00:00Z",
            CompositionDefaults::default(),
            EndedReason::Normal,
        )
    }

    /// Stand-in for the v1 ffmpeg remux on the legacy orphan path — just
    /// copies bytes. Used by tests that exercise `.partial.mkv` adoption.
    fn copy_remux(src: &Path, dst: &Path) -> Result<()> {
        std::fs::copy(src, dst).map_err(|e| CoreError::Io {
            path: dst.to_path_buf(),
            source: e,
        })?;
        Ok(())
    }

    #[test]
    fn prepare_segment_path_creates_dir_and_returns_dot_partial_mov_under_it() {
        let f = course_folder();
        let (id, path) = prepare_segment_path(f.path(), "vid-1").unwrap();

        assert!(!id.is_empty());
        let expected_dir = f.path().join("videos").join("vid-1").join("segments");
        assert!(expected_dir.is_dir(), "segments dir was not created");
        assert_eq!(path.parent().unwrap(), expected_dir);

        let name = path.file_name().unwrap().to_str().unwrap();
        assert_eq!(name, format!("{id}.{PARTIAL_SUFFIX}"));
        assert!(name.ends_with(".partial.mov"));
    }

    #[test]
    fn prepare_segment_path_gives_distinct_ids_for_successive_calls() {
        let f = course_folder();
        let (a, _) = prepare_segment_path(f.path(), "vid-1").unwrap();
        let (b, _) = prepare_segment_path(f.path(), "vid-1").unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn finalize_segment_renames_partial_to_mov_and_writes_sidecar() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"FAKE-MOV").unwrap();

        let seg = finalize_segment(f.path(), "vid-1", &id, sample_sidecar()).unwrap();
        assert_eq!(seg.id, id);
        assert_eq!(seg.video_id, "vid-1");
        assert_eq!(
            seg.path,
            PathBuf::from("videos")
                .join("vid-1")
                .join("segments")
                .join(format!("{id}.{SEGMENT_EXT}"))
        );

        assert!(!partial.exists(), "partial should be gone after rename");
        let final_path = f.path().join(&seg.path);
        assert!(final_path.is_file(), "final .mov should exist");
        // Content was rename, not re-encoded.
        assert_eq!(std::fs::read(&final_path).unwrap(), b"FAKE-MOV");

        // Sidecar landed next to it.
        let sidecar = read_sidecar(f.path(), "vid-1", &id).unwrap().unwrap();
        assert_eq!(sidecar, sample_sidecar());
    }

    #[test]
    fn finalize_segment_is_idempotent_if_already_finalised() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id, sample_sidecar()).unwrap();

        // Replay Keep — the partial is gone, the final is in place. We
        // accept this and rewrite the sidecar.
        let seg = finalize_segment(f.path(), "vid-1", &id, sample_sidecar()).unwrap();
        assert_eq!(seg.id, id);
    }

    #[test]
    fn finalize_segment_errors_when_neither_partial_nor_final_exists() {
        let f = course_folder();
        let result = finalize_segment(f.path(), "vid-1", "no-such-segment", sample_sidecar());
        assert!(matches!(result, Err(CoreError::SegmentNotFound(_))));
    }

    #[test]
    fn discard_partial_removes_new_shape_partial() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();

        discard_partial(f.path(), "vid-1", &id).unwrap();
        assert!(!partial.exists());
    }

    #[test]
    fn discard_partial_removes_legacy_partial_mkv_too() {
        let f = course_folder();
        let dir = segments_dir(f.path(), "vid-1");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join(format!("legacy-id.{LEGACY_PARTIAL_SUFFIX}"));
        std::fs::write(&legacy, b"v1").unwrap();

        discard_partial(f.path(), "vid-1", "legacy-id").unwrap();
        assert!(!legacy.exists());
    }

    #[test]
    fn discard_partial_is_silent_when_file_is_already_missing() {
        let f = course_folder();
        discard_partial(f.path(), "vid-1", "ghost").unwrap();
    }

    #[test]
    fn discard_partial_does_not_touch_finalised_segments() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id, sample_sidecar()).unwrap();

        discard_partial(f.path(), "vid-1", &id).unwrap();
        let final_path = segments_dir(f.path(), "vid-1").join(format!("{id}.{SEGMENT_EXT}"));
        assert!(final_path.is_file(), "finalised file must be preserved");
    }

    #[test]
    fn list_segments_returns_empty_when_segments_dir_missing() {
        let f = course_folder();
        let segs = list_segments(f.path(), "vid-1").unwrap();
        assert!(segs.is_empty());
    }

    #[test]
    fn list_segments_includes_new_mov_finals_skips_partials_sidecars_and_dotfiles() {
        let f = course_folder();
        let (id_keep, p_keep) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_keep, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id_keep, sample_sidecar()).unwrap();

        // A still-in-progress one, a sidecar (the one we just wrote), and a
        // dotfile that should all be ignored.
        let (_, p_partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_partial, b"x").unwrap();
        std::fs::write(
            segments_dir(f.path(), "vid-1").join(".DS_Store"),
            b"x",
        )
        .unwrap();

        let segs = list_segments(f.path(), "vid-1").unwrap();
        let ids: Vec<_> = segs.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec![id_keep.as_str()]);
    }

    #[test]
    fn list_segments_picks_up_legacy_mp4_finals_for_v1_back_compat() {
        let f = course_folder();
        let dir = segments_dir(f.path(), "vid-1");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("v1-id.mp4");
        std::fs::write(&legacy, b"v1").unwrap();

        let segs = list_segments(f.path(), "vid-1").unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].id, "v1-id");
        assert_eq!(
            segs[0].path,
            PathBuf::from("videos").join("vid-1").join("segments").join("v1-id.mp4")
        );
    }

    #[test]
    fn list_segments_returns_both_shapes_when_both_exist() {
        let f = course_folder();
        let dir = segments_dir(f.path(), "vid-1");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.mp4"), b"v1").unwrap();
        std::fs::write(dir.join("b.mov"), b"v2").unwrap();

        let ids: Vec<_> = list_segments(f.path(), "vid-1")
            .unwrap()
            .into_iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn scan_orphans_finds_partial_mov_and_partial_mkv_across_videos() {
        let f = course_folder();
        let (id_new, p_new) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_new, b"x").unwrap();

        let dir_legacy = segments_dir(f.path(), "vid-2");
        std::fs::create_dir_all(&dir_legacy).unwrap();
        let legacy = dir_legacy.join(format!("legacy.{LEGACY_PARTIAL_SUFFIX}"));
        std::fs::write(&legacy, b"v1").unwrap();

        // Finalised segments must NOT show up as orphans.
        let (id_done, p_done) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&p_done, b"x").unwrap();
        let _ = finalize_segment(f.path(), "vid-1", &id_done, sample_sidecar()).unwrap();

        let mut orphans = scan_orphans(f.path()).unwrap();
        orphans.sort_by(|a, b| a.id.cmp(&b.id));
        let got_ids: Vec<_> = orphans.iter().map(|o| o.id.clone()).collect();
        let mut expected = vec![id_new.clone(), "legacy".to_string()];
        expected.sort();
        assert_eq!(got_ids, expected);

        // Sanity-check the path shapes survived to the relative output.
        for o in &orphans {
            assert!(o.path.starts_with("videos"));
            assert!(f.path().join(&o.path).is_file());
            let name = o.path.file_name().unwrap().to_str().unwrap();
            assert!(name.ends_with(PARTIAL_SUFFIX) || name.ends_with(LEGACY_PARTIAL_SUFFIX));
        }
    }

    #[test]
    fn scan_orphans_returns_empty_when_videos_dir_absent() {
        let f = course_folder();
        let orphans = scan_orphans(f.path()).unwrap();
        assert!(orphans.is_empty());
    }

    #[test]
    fn write_then_read_sidecar_round_trips() {
        let f = course_folder();
        std::fs::create_dir_all(segments_dir(f.path(), "vid-1")).unwrap();
        let s = sample_sidecar();
        write_sidecar(f.path(), "vid-1", "seg-1", &s).unwrap();
        let back = read_sidecar(f.path(), "vid-1", "seg-1").unwrap().unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn read_sidecar_returns_none_for_v1_segments_without_one() {
        let f = course_folder();
        std::fs::create_dir_all(segments_dir(f.path(), "vid-1")).unwrap();
        // A v1 .mp4 with no sidecar — explicitly the "no sidecar" case.
        std::fs::write(segments_dir(f.path(), "vid-1").join("v1.mp4"), b"x").unwrap();
        let got = read_sidecar(f.path(), "vid-1", "v1").unwrap();
        assert!(got.is_none(), "v1 segments without sidecars must not error");
    }

    #[test]
    fn adopt_orphan_new_shape_renames_to_mov_and_writes_sidecar() {
        let f = course_folder();
        let (id, partial) = prepare_segment_path(f.path(), "vid-1").unwrap();
        std::fs::write(&partial, b"recovered").unwrap();
        let mut s = sample_sidecar();
        s.ended_reason = EndedReason::Crashed;

        let seg = adopt_orphan(f.path(), "vid-1", &id, s.clone(), |_, _| {
            panic!("legacy remux must not run for new-shape orphans")
        })
        .unwrap();
        assert!(seg.path.to_string_lossy().ends_with(".mov"));

        let back = read_sidecar(f.path(), "vid-1", &id).unwrap().unwrap();
        assert_eq!(back.ended_reason, EndedReason::Crashed);
    }

    #[test]
    fn adopt_orphan_legacy_partial_mkv_remuxes_to_mp4_and_writes_no_sidecar() {
        let f = course_folder();
        let dir = segments_dir(f.path(), "vid-1");
        std::fs::create_dir_all(&dir).unwrap();
        let legacy_partial = dir.join(format!("legacy-id.{LEGACY_PARTIAL_SUFFIX}"));
        std::fs::write(&legacy_partial, b"v1-bytes").unwrap();

        let seg = adopt_orphan(
            f.path(),
            "vid-1",
            "legacy-id",
            sample_sidecar(),
            copy_remux,
        )
        .unwrap();

        // v1 orphan adoption preserves v1 behaviour: produces a .mp4, no sidecar.
        assert!(seg.path.to_string_lossy().ends_with(".mp4"));
        assert!(!legacy_partial.exists());
        let final_path = f.path().join(&seg.path);
        assert!(final_path.is_file());
        assert!(
            read_sidecar(f.path(), "vid-1", "legacy-id").unwrap().is_none(),
            "v1 orphan adoption deliberately does not write a sidecar (Phase 7 polish)"
        );
    }

    // --- Issue #38: per-Take orphan recovery -------------------------------

    fn write_partial(folder: &std::path::Path, video_id: &str, segment_id: &str, ext: &str) {
        let segs = segments_dir(folder, video_id);
        std::fs::create_dir_all(&segs).unwrap();
        std::fs::write(segs.join(format!("{segment_id}.{ext}")), b"FAKE-PARTIAL").unwrap();
    }

    #[test]
    fn take_marker_round_trips_via_disk() {
        let f = course_folder();
        let marker = TakeMarker {
            schema_version: TakeMarker::SCHEMA_VERSION,
            take_id: "take-1".into(),
            video_id: "vid-1".into(),
            recorded_at: "2026-05-27T01:00:00Z".into(),
            scene_id: Some("scene-xyz".into()),
            sources: vec![TakeMarkerSource {
                segment_id: "seg-a".into(),
                request: dummy_request(SourceRole::Screen),
            }],
        };
        write_take_marker(f.path(), "vid-1", &marker).unwrap();
        let back = read_take_marker(f.path(), "vid-1", "take-1").unwrap().unwrap();
        assert_eq!(back, marker);
    }

    #[test]
    fn remove_take_marker_is_idempotent() {
        let f = course_folder();
        remove_take_marker(f.path(), "vid-1", "no-such-take").unwrap();
    }

    #[test]
    fn scan_orphan_takes_groups_partials_by_take_id_via_marker() {
        let f = course_folder();
        // Two partials sharing a takeId — that's what a multi-source
        // crashed Take looks like on disk.
        write_partial(f.path(), "vid-1", "seg-screen", PARTIAL_SUFFIX);
        write_partial(f.path(), "vid-1", "seg-cam", PARTIAL_SUFFIX);
        let marker = TakeMarker {
            schema_version: TakeMarker::SCHEMA_VERSION,
            take_id: "take-multi".into(),
            video_id: "vid-1".into(),
            recorded_at: "2026-05-27T01:00:00Z".into(),
            scene_id: None,
            sources: vec![
                TakeMarkerSource {
                    segment_id: "seg-screen".into(),
                    request: dummy_request(SourceRole::Screen),
                },
                TakeMarkerSource {
                    segment_id: "seg-cam".into(),
                    request: dummy_request(SourceRole::Camera),
                },
            ],
        };
        write_take_marker(f.path(), "vid-1", &marker).unwrap();

        let takes = scan_orphan_takes(f.path()).unwrap();
        assert_eq!(takes.len(), 1, "two partials with one marker = one Take");
        let take = &takes[0];
        assert_eq!(take.take_id, "take-multi");
        assert_eq!(take.segments.len(), 2);
        let roles: Vec<SourceRole> = take.segments.iter().map(|s| s.source_role).collect();
        assert!(roles.contains(&SourceRole::Screen));
        assert!(roles.contains(&SourceRole::Camera));
    }

    #[test]
    fn scan_orphan_takes_surfaces_v1_legacy_partials_as_single_segment_takes() {
        let f = course_folder();
        write_partial(f.path(), "vid-1", "legacy-id", LEGACY_PARTIAL_SUFFIX);
        let takes = scan_orphan_takes(f.path()).unwrap();
        assert_eq!(takes.len(), 1);
        assert_eq!(takes[0].segments.len(), 1);
        assert!(takes[0].take_id.starts_with("legacy-"));
    }

    #[test]
    fn scan_orphan_takes_handles_mixed_video_and_audio_partials() {
        let f = course_folder();
        write_partial(f.path(), "vid-1", "seg-screen", PARTIAL_SUFFIX);
        write_partial(f.path(), "vid-1", "seg-mic", PARTIAL_SUFFIX_AUDIO);
        let marker = TakeMarker {
            schema_version: TakeMarker::SCHEMA_VERSION,
            take_id: "take-mixed".into(),
            video_id: "vid-1".into(),
            recorded_at: "2026-05-27T01:00:00Z".into(),
            scene_id: None,
            sources: vec![
                TakeMarkerSource {
                    segment_id: "seg-screen".into(),
                    request: dummy_request(SourceRole::Screen),
                },
                TakeMarkerSource {
                    segment_id: "seg-mic".into(),
                    request: dummy_request(SourceRole::Microphone),
                },
            ],
        };
        write_take_marker(f.path(), "vid-1", &marker).unwrap();
        let takes = scan_orphan_takes(f.path()).unwrap();
        assert_eq!(takes.len(), 1);
        let roles: Vec<SourceRole> = takes[0].segments.iter().map(|s| s.source_role).collect();
        assert!(roles.contains(&SourceRole::Screen));
        assert!(roles.contains(&SourceRole::Microphone));
    }
}
