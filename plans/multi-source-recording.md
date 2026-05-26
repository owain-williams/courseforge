# Plan: Multi-source recording and multi-track composition

> Source PRD: [`SRS.html`](../SRS.html) (v0.2 — §3.2, §3.3, §4.3, §4.5, §4.6, §4.7).
> Architectural rationale: [ADR-0002](../docs/adr/0002-multi-source-capture-via-in-process-screencapturekit.md), [ADR-0003](../docs/adr/0003-multi-track-timeline-with-video-level-ripple-cuts.md).
> Domain language: [`CONTEXT.md`](../CONTEXT.md).

## Architectural decisions

Durable decisions referenced by every phase:

- **Capture backend**: in-process Rust via `objc2` bindings to ScreenCaptureKit (screen, window, system_audio) and AVFoundation (camera, microphone). Per-source `AVAssetWriter` outputs. Frame-accurate cross-source sync via shared `CMSampleBuffer` PTS.
- **Capture request shape**: `Vec<CaptureRequest>` where each entry is `{ role, deviceId, label, defaults }`. Replaces the v1 `CaptureSources { microphone, system_audio, webcam }` boolean trio.
- **Source Role taxonomy**: closed set `{ screen, window, camera, microphone, system_audio }`. Multiple-of-same-role per Take permitted.
- **Take lifecycle**: atomic Start; mid-recording per-source failure drops that source while the rest continue; Take-level Keep / Discard.
- **On-disk additions**:
  - `scenes.json` — sibling to `course.json` at the Course Folder root.
  - Per-Segment sidecars at `videos/<vid>/segments/<sid>.json` carrying `{ takeId, sourceRole, device, recordedAt, defaults, endedReason }`.
  - `course.json` gains an optional `frameSize: { width, height, fps }` field (default 1920×1080 30fps when absent).
- **EDL schema**: `edits.json` `schemaVersion` 1 → 2. Tracks as first-class persistent entities; Clips on Tracks; video-level ripple Cuts. Command log extended with `AddTrack`, `ReorderTracks`, `SetTrackProperty`, `AddClip`, `MoveClip`, `RemoveClip`, `SetClipProperty`, `LinkClips`, `UnlinkClips` alongside the existing `AddCut` / `Undo` / `Redo`.
- **Transcript schema**: `transcript.json` `schemaVersion` 1 → 2 — `{ segmentTranscripts: [ { segmentId, words[…source-time] } ] }`. Video-timeline word list derived at read.
- **Linking rule**: Clips that share a `takeId` are linked by default; selection / move applies to all in the link group; user can explicitly Unlink.
- **Compositor**: one pure `core::compose` module derives a frame layout description from `(timeline state, output frame, time t)`. Two renderers consume it — CSS+canvas preview and ffmpeg `overlay`+`amix` export.
- **Scale semantics**: canvas-relative. `scale = 1.0` means the Clip fills the Video's output frame at the source's aspect ratio.
- **Audio gain**: stored and surfaced in dB; range −∞ to +12 dB; 0 dB = recorded level.
- **Migration**: lazy / on-read. v1 Course Folders are readable by v2 builds without rewriting on disk; v2-shape writes happen only on user mutation.

---

## Phase 1: In-process single-source recorder via ScreenCaptureKit

**User stories / requirements covered**: FR-3.11 (permissions), and the SCK / AVAssetWriter backend that FR-3.2 / FR-3.3 / FR-3.4 will build on.

### What to build

Replace the ffmpeg-child recorder backend with an in-process Rust subsystem using `objc2` bindings to ScreenCaptureKit + AVCaptureSession + AVAssetWriter, capturing a single source (the existing v1 behaviour: screen + optional mic, atomically). The on-disk artifact changes — instead of a `.partial.mkv` remuxed to `.mp4`, the recorder writes a `.mov` directly via AVAssetWriter, and a per-Segment sidecar JSON lands alongside. The `RecorderBackend` trait accepts `Vec<CaptureRequest>` but is only exercised with one entry in this phase. Camera and Microphone TCC bridging via `objc2` replaces the optimistic-Granted placeholders.

The user-facing flow is unchanged: hit Record on a Video slot, recording starts, Pause / Resume / Stop work, Keep / Discard work, the finished `.mov` plays in the editor.

### Acceptance criteria

- [ ] `RecorderBackend` trait signature accepts `Vec<CaptureRequest>` with `{ role, deviceId, label, defaults }`; `CaptureSources` boolean trio is removed from the public API surface.
- [ ] On macOS, a recording started from the existing Record button uses the new in-process SCK + AVCaptureSession backend; no `ffmpeg` child is spawned for capture.
- [ ] The recording writes a single AVAssetWriter `.mov` output to `videos/<vid>/segments/<sid>.mov` plus a sidecar `<sid>.json` containing `{ takeId, sourceRole, device, recordedAt, defaults, endedReason }`.
- [ ] The Remuxer pipeline (`.partial.mkv` → `.mp4`) is removed for the recorder path; `finalize_segment` becomes a rename / sidecar-write step.
- [ ] Pause / Resume / Stop work and result in an atomic PTS marker that closes the AssetWriter cleanly; the resulting `.mov` plays in WebKit `<video>`.
- [ ] Camera and Microphone permission status are queried via `AVCaptureDevice.authorizationStatus` through `objc2`; status flows through the existing `PermissionStatus` enum.
- [ ] Permission failures surface as actionable errors with a deep-link to the relevant Privacy & Security pane (existing UX preserved).
- [ ] Existing v1 Course Folders open and play back; the v1 `.mp4` Segments are readable by the new editor unchanged.
- [ ] Integration test on macOS: record a ~3-second screen+mic clip, assert the `.mov` exists, its sidecar JSON validates, and ffprobe reports a positive duration with both video and audio tracks.

---

## Phase 2: Scenes — multi-source capture with Scene Editor

**User stories / requirements covered**: FR-3.2, FR-3.3, FR-3.4, FR-3.6, FR-3.8, FR-3.9, FR-3.10.

### What to build

Introduces the Scene as the unit of capture configuration. A Scene Editor screen lets the user author named, reusable presets at the Course level — pick Source Roles, bind each to a specific device, set initial composition properties (`position`, `scale`, `opacity`, `audioGain`) on a canvas preview. Scenes persist to `scenes.json` at the Course Folder root. A Video can pin a default Scene; "Record" uses that, and "Record with…" opens a picker.

The recorder backend grows from "one source per Take" to "N sources per Take". All N AVAssetWriters share a `CMSampleBuffer` clock so cross-source sync is frame-accurate. Start is atomic (any single-source failure aborts the whole Take and cleans up partials). Mid-recording per-source failure drops that source — its sidecar records `endedReason: sourceFailed` with the failure PTS — while the rest of the Take continues. Keep and Discard apply to the whole Take. Per-Take orphan recovery on next launch groups partial AssetWriter outputs by `takeId` and offers Import / Discard for the Take.

For this phase, the editor's view of the resulting Segments is unchanged — the user sees a list of Segments per Video (the first one of the Take), and Phase 3 turns them into Clips on Tracks. The deliverable is "you can record a multi-source Take cleanly to N files on disk".

### Acceptance criteria

- [ ] Scene Editor screen reachable from the Course window; lists existing Scenes, lets the user create, rename, duplicate, and delete a Scene.
- [ ] Within a Scene the user can add a row per Source Role from the closed taxonomy, bind it to a specific device (chosen from a live device list), set per-source initial composition properties on a canvas preview matching the Course's output canvas (1920×1080 default at this point — Phase 4 makes it configurable).
- [ ] Exactly one source per Scene may be marked as the Transcript Source.
- [ ] Scenes persist to `scenes.json` at the Course Folder root with `schemaVersion: 1`. Missing `scenes.json` = no Scenes (acceptable).
- [ ] A Video may pin a default Scene; the picker reflects this.
- [ ] Hitting Record on a Video uses the pinned Scene; "Record with…" opens a Scene picker. Hard-fail if any Scene device is missing at Start time, with a per-source diagnostic.
- [ ] Start is atomic: if any single source fails to initialise (device missing, permission denied, SCK filter rejection), the entire Take aborts; no Segment files or sidecars are left behind.
- [ ] Mid-recording per-source failure ends that source's Segment cleanly; the failing source's sidecar carries `endedReason: sourceFailed` and the failure PTS; the rest of the Take continues.
- [ ] Pause / Resume / Stop are atomic across all N sources in the Take.
- [ ] Keep promotes all N Segments and their sidecars to final names; Discard removes them all. One decision applies to the whole Take.
- [ ] After a crash mid-Take, on next Course open the orphan scanner groups partial AssetWriter outputs by `takeId` (read from sidecars) and offers Import / Discard per Take.
- [ ] Cross-source sync at capture is frame-accurate: in an integration test that records two AVCaptureSession sources, the first samples of both Segments are within one frame interval of each other (per PTS).
- [ ] Integration test on macOS: record a 3-source Take (screen + camera + mic) via a Scene, verify three `.mov` / `.m4a` files in `segments/`, all with sidecars sharing the same `takeId` and `recordedAt`.

---

## Phase 3: Multi-track timeline data model + auto-arrange on Keep

**User stories / requirements covered**: FR-3.5, FR-6.4 (composition properties as persistent values, no UI yet), and the §3.2 multi-track data model.

### What to build

`edits.json` schema advances from v1 (cuts only) to v2 (Tracks, Clips, Cuts, command log extended). Tracks are persistent entities with `{ id, kind, order, name, lock, solo, mute }`. Clips reference Segments and carry `{ trackId, segmentId, startOnTimeline, inPoint, outPoint, position, scale, opacity, audioGain, visible }`. The command log gains `AddTrack` / `ReorderTracks` / `SetTrackProperty` / `AddClip` / `MoveClip` / `RemoveClip` / `SetClipProperty` / `LinkClips` / `UnlinkClips`; existing `AddCut` / `Undo` / `Redo` stay.

Auto-arrange on Keep implements Behaviour A from ADR-0003 — when a Take is Kept, its N Segments become Clips on N Tracks matched by Source Role, placed at the end of the Video's existing timeline; new Tracks are created when no matching one exists. Linked Clips by `takeId` are wired in the data model (Phase 7 surfaces the UI for Unlink).

A v1 → v2 lazy migration adapter wraps the read path: a v1 log replays as the implicit single-Track / single-Clip case so existing Courses still open. v2 writes only happen when the user mutates.

The editor visually shows the multi-track timeline (rows per Track, blocks per Clip) — but the preview at this point is still single-track (the topmost video Clip plays). Phase 4 makes the compositor real.

### Acceptance criteria

- [ ] `edits.json` v2 round-trips: write log → read → replay → equal in-memory state.
- [ ] All new command variants exist with replay rules; Undo / Redo treat them as first-class.
- [ ] v1 `edits.json` files load under v2 builds and replay as the implicit single-Track / single-Clip case; v1 files are not rewritten on read.
- [ ] On Keep, the N Segments of a Take materialise as N Clips on Tracks matched by Source Role; if no matching Track exists, a new one is appended (video Tracks first by `order`, then audio Tracks).
- [ ] New Clips' `startOnTimeline` equals the end of the Video's existing timeline; their initial composition properties are copied from the Segment's sidecar `defaults`.
- [ ] Clips with the same `takeId` are linked-by-default in the in-memory state (the `Linked Clips` rule is queryable by the editor; the UI to break the link is Phase 7).
- [ ] The editor renders the multi-track timeline: rows per Track in `order`, blocks per Clip at the right time / duration.
- [ ] Users can drag a Clip in time on its Track, delete a Clip, reorder Tracks; each action persists as a command log append.
- [ ] Single-Segment v1 Courses still play and edit correctly under v2 — same visible behaviour as Phase 0.
- [ ] Integration test: record a 2-source Take via Phase 2, Keep it, assert two Tracks appear in the v2 `edits.json` with one Clip each.

---

## Phase 4: Shared `core::compose` model + preview + export compositor

**User stories / requirements covered**: FR-7.4, FR-7.5; the rendering side of FR-3.5 and FR-6.4.

### What to build

The pure `core::compose` module: given `(timeline state, output frame, time t)`, return a frame layout description listing, per visible Track in `order`, the Clip currently showing and its rendered `{ x, y, w, h, opacity, audioGainDb }`. The math is unit-testable without rendering.

A per-Course output canvas (`frameSize`) lands on `course.json` (1920×1080 30fps default; user-configurable via a new Course Settings panel). All composition properties (`scale`, `position`) are interpreted in canvas coordinates; `scale = 1.0` means the Clip fills the canvas at the source's aspect ratio.

Two renderers consume the layout description:

- **Preview**: N `<video>` elements stacked in the editor, positioned via CSS transforms from the layout, audio playing out of the elements natively; a master-clock loop ticks each element's `currentTime` to stay synced.
- **Export**: a Rust → ffmpeg `-filter_complex` graph with `overlay` + `amix` nodes, replacing the v1 `trim + concat` pipeline. Cancellation and progress contracts are unchanged.

This phase doesn't introduce ripple Cuts onto the multi-track world — Phase 5 does. Here the compositor handles the no-Cut case end to end, which is enough to demo "I record a multi-source Take and see the layered composite in preview and in the exported MP4".

### Acceptance criteria

- [ ] `core::compose` derives a frame layout description from a (timeline, output frame, t) input; tested with golden cases (no Clips, one Clip filling, two Clips with overlay, opacity = 0 hiding a layer).
- [ ] `course.json` gains an optional `frameSize: { width, height, fps }`; missing or v1 files default to 1920×1080 30fps; Course Settings UI lets the user change it; new value is persisted on next save.
- [ ] `scale = 1.0` renders a Clip filling the canvas at the source's aspect ratio; `scale = 0.25` renders it at 25% of the canvas; tested both in preview and export.
- [ ] Preview renderer stacks N `<video>` elements positioned via CSS transforms from the layout; a master-clock loop syncs their `currentTime` within one frame on play / pause / seek.
- [ ] Audio in preview plays out of the `<video>` elements natively; muted Clips (`audioGain == −∞`) produce no audio; gain values map from dB to the element's `volume` (or Web Audio gain node) within the supported range.
- [ ] Export renderer translates the layout description into an ffmpeg filter graph with `overlay` for video and `amix` for audio; the exported MP4 plays in a standard player with the expected layout.
- [ ] Frame-by-frame comparison test (or first-frame snapshot test) shows preview and export agree on Clip positions to within a small tolerance.
- [ ] v1 single-Segment Videos still preview and export correctly (the trivial one-Track / one-Clip case in `core::compose`).
- [ ] Integration test: record a 2-source Take, Keep, export → exported MP4 contains both sources composited at their Scene-defined defaults.

---

## Phase 5: Video-level ripple Cuts on the multi-track timeline

**User stories / requirements covered**: FR-5.3, FR-5.4, FR-5.5 (re-asserted under the multi-track model).

### What to build

`AddCut` semantics on the v2 EDL: a Cut is a time range on the Video timeline; at render time, every Clip overlapping its span has the corresponding sub-range dropped, and downstream Clips slide left to close the gap (ripple). The `core::compose` math gains keep-range derivation across the multi-track timeline; the export pipeline composes only the kept sub-ranges. The existing transcript-driven cut UI (and any timeline-drag cut affordance) continues to work, now driving multi-track cuts.

The transcript view still drives cut selection from FR-5.3 but operates against the Video-timeline-derived transcript (a transient stitch of the per-Segment transcripts from the Transcript Source — full per-Segment storage is Phase 6).

### Acceptance criteria

- [ ] `AddCut` in the v2 command log records a `{ startSec, endSec }` range on the Video timeline; replay reconstructs the active Cut set.
- [ ] `core::compose` produces keep-ranges across the multi-track timeline correctly: every Clip overlapping the Cut's span loses the corresponding sub-range; downstream Clips on every Track slide left by the cut width.
- [ ] Touching / overlapping Cuts are merged before render (no zero-width slivers).
- [ ] Edited duration of the Video timeline equals (sum of Clip on-timeline spans) − (sum of merged Cut widths), clipped to the timeline.
- [ ] Undo / Redo on Cuts works under the v2 schema with the same unlimited-history guarantee as v1.
- [ ] Transcript-driven cut UX from v1 continues to work; clicking a word range and pressing Cut produces an `AddCut` that ripples across all Tracks.
- [ ] Preview and export agree on what's dropped — the exported MP4's duration matches the preview-reported edited duration.
- [ ] Integration test: 2-source Take with two takes appended, Cut spanning the boundary between Take 1's screen and Take 2's screen → both Tracks (screen and camera) drop the span; audio Track does too if a `mic` Clip overlaps.

---

## Phase 6: Per-Segment transcripts + Transcript Source filtering

**User stories / requirements covered**: FR-5.1, FR-5.6, FR-5.7, FR-5.8.

### What to build

`transcript.json` schema advances from v1 (flat words on the Segment timeline) to v2 (`{ segmentTranscripts: [ { segmentId, words[…source-time] }, … ] }`). Words are stored in their Segment's source time; the Video-timeline view is derived at read using the current Clip placements (`startOnTimeline`, `inPoint`, `outPoint`) and the active Cuts. The transcription enqueue logic only feeds Segments produced by sources marked as the Transcript Source on the Scene used to capture the Take (read from the per-Segment sidecar). Words that fall inside Cut spans are dropped from the derived view; words that straddle a Cut boundary are intersected (current `srt_from_transcript` behaviour, carried forward).

The existing transcript editor screen (synced word highlight, sentence/word selection, cut-from-transcript) is rewired to consume the derived word stream rather than the single-source flat list.

A v1 `transcript.json` (flat words) is wrapped on read as a single `segmentTranscripts` entry keyed to the Video's only Segment, so v1 Courses keep working. The v1 file is not rewritten on read.

### Acceptance criteria

- [ ] `transcript.json` v2 round-trips (`schemaVersion: 2`); written only on first v2 transcription.
- [ ] v1 transcript files are wrapped at read into the v2 shape (single `segmentTranscripts` entry); v1 files on disk are not rewritten until the user mutates.
- [ ] Only Segments whose sidecar marks them as the Take's Transcript Source are enqueued for transcription; other audio sources (e.g. `system_audio`) are skipped.
- [ ] Derived Video-timeline words correctly account for Clip `startOnTimeline`, `inPoint`, `outPoint`, and active Cuts; words inside a Cut do not appear; words straddling a Cut boundary are handled per the existing intersection rules.
- [ ] Moving a Clip in Assembly shifts its words on the derived view but does not require re-running transcription (no new transcript file is written).
- [ ] The existing synced-word-highlight playback view works against the derived stream.
- [ ] Transcript-driven Cut UI (Phase 5) operates on the derived stream and produces ripple Cuts on the Video timeline.
- [ ] `.srt` export uses the derived stream and matches the edited timeline (existing AC carried forward).
- [ ] Integration test: 2-Take Video with mic as Transcript Source in both Takes → derived stream has words from both Takes' mics at correctly-offset Video times; system_audio Segments are not transcribed.

---

## Phase 7: Linked Clips + dB audio gain UI + final polish

**User stories / requirements covered**: FR-6.4 (UI side: composition property editors); the editor side of the `Linked Clips` rule from ADR-0003.

### What to build

Surfaces the editor-side UX for the data and rules built in earlier phases:

- A Properties panel / inspector for the selected Clip showing `position`, `scale`, `opacity`, `audioGain` (the latter as a dB fader from −∞ to +12 dB), and `visible`. Edits write to the EDL via `SetClipProperty` commands; preview and export update live.
- Linked Selection: selecting a Clip selects every Clip sharing its `takeId`; moves apply to all in the link group; Unlink and Link commands surfaced via the command log (`LinkClips` / `UnlinkClips` from Phase 3) and a menu action.
- Per-Track properties UI: lock, solo, mute, name (toggles writing `SetTrackProperty` commands).
- Final migration polish: `.partial.mkv` orphans from the v1 ffmpeg-child recorder remain importable through the per-Take orphan flow (they appear as single-source Takes); a one-time migration helper rewrites v1 → v2 on the next user mutation as planned.
- Documentation pass: in-tree help / first-run hints surface the four Workflow Phases so the user maps Capture / Assembly / Editing to the visible UI modes.

### Acceptance criteria

- [ ] Selecting a Clip shows a Properties panel; editing `position` / `scale` / `opacity` / `visible` updates preview and persists to the EDL.
- [ ] `audioGain` UI is a dB fader (−∞ to +12 dB, 0 dB centered); the stored value is the dB number; preview and export apply it consistently.
- [ ] Selecting a Clip selects every Clip sharing its `takeId`; moving one moves all by the same delta; the same applies to copy / delete.
- [ ] Unlink command on a link group ungroups them; subsequent moves are independent; Re-link via the menu re-groups.
- [ ] Per-Track lock / solo / mute / name controls work and persist via `SetTrackProperty`.
- [ ] v1 `.partial.mkv` orphans from before Phase 1 still appear in the per-Take orphan flow as single-source Takes; importing them produces a single Clip on one Track.
- [ ] Help / first-run hint introduces the four Workflow Phases mapped to the visible UI.
- [ ] Final integration test: full flow — author a Scene, record a Take, Keep, the Take appears as linked Clips on Tracks, nudge one Clip and confirm the rest move, Unlink one, adjust per-Clip gain in dB, add a Cut, preview, export → exported MP4 reflects every adjustment.
