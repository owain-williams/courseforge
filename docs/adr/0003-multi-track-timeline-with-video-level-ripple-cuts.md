# Multi-track timeline with video-level ripple cuts and `takeId`-linked Clips

## Context

The v1 editor models a Video as a single source MP4 trimmed by cuts on its own timeline. The EDL (`edits.json`) is a command log of `AddCut` / `Undo` / `Redo`; replay reconstructs the active cut set. The exporter takes the source MP4, the keep-ranges derived from cuts, and renders by `trim + concat`. Transcript words live in source-timeline time and the transcript-driven cut feature uses those timestamps directly.

ADR-0002 makes Capture produce N peer Segments per Take, each with its own media file and a sidecar describing role / device / Scene-derived defaults. The editor therefore has to evolve from "trim one source" to "stack and overlay N sources" while keeping the cut and transcript affordances the user already relies on.

The decision shape is: what's the on-disk representation of a multi-track timeline, what does a "cut" mean when there are multiple Tracks, how does the rendering pipeline (preview + export) stay consistent, and how do v1 Course Folders continue to work.

## Decision

**The Video's persistent state grows to model Tracks, Clips, and Video-level ripple Cuts. The EDL stays a command log (extended with new variants). Preview and export are driven by one shared `core::compose` model so they cannot diverge.**

Concretely:

### Tracks are first-class

A `Track` has `{ id, kind: video | audio, order, name, lock, solo, mute }`. Tracks own ordering and per-Track metadata. A Video's timeline is `Vec<Track>` rendered in `order`.

### Clips are placements of Segments on Tracks

A `Clip` is `{ id, trackId, segmentId, startOnTimeline, inPoint, outPoint, position, scale, opacity, audioGain, visible }`. The Segment file is immutable; the Clip's properties are project state. Multiple Clips may reference the same Segment.

### Video-level ripple Cuts

A `Cut` is a time range on the *Video's assembled timeline*. At render time, every Clip overlapping the cut span has the corresponding sub-range dropped, and **downstream Clips slide left to close the gap** — there is no "lift" affordance in v1.5. This matches the user's intent ("cut that ten seconds where I said the wrong word") across screen, camera, and mic in one action.

### `takeId`-linked Clips

Clips sharing a `takeId` (i.e. the N Clips born from one Take) are **linked by default** — selecting or moving one applies to all. The user can explicitly **Unlink** in the editor when they intend to drift sources apart. This is what preserves the frame-accurate sync ADR-0002 paid for at Capture.

### Command-log EDL extended

`edits.json` stays a command log; `schemaVersion` advances from 1 to 2. New commands: `AddTrack`, `ReorderTracks`, `SetTrackProperty`, `AddClip`, `MoveClip`, `RemoveClip`, `SetClipProperty`, `LinkClips`, `UnlinkClips`. Existing `AddCut` / `Undo` / `Redo` stay. Replay reconstructs Tracks, Clips, and the active cut set in one pass. We keep unlimited free undo by extending the existing append-and-replay machinery.

### One shared compositor model

A pure `core::compose` module derives, from `(timeline state, output frame, time t)`, a **frame layout description**: for each visible Track in `order`, "this Clip's source-time `s` is showing at `{x, y, w, h, opacity, audioGainDb}`". Two renderers consume this:

- **Preview renderer (frontend, Capture/Assembly/Editing views)** — N `<video>` elements stacked, positioned via CSS transforms from the layout description, `currentTime` driven from a master clock that ticks the elements to stay synced. Audio plays out of the elements natively (the browser mixes).
- **Export renderer (Rust → ffmpeg)** — the same layout description compiled to an ffmpeg `-filter_complex` graph with `overlay` and `amix` nodes.

The compose math is small (per-frame: N affine transforms + alpha + audio gain) and unit-testable without rendering anything.

### Output canvas

The Video's output frame is **per-Course Project Settings**, default `1920×1080 30fps`, persisted on `course.json` (one of the few course-skeleton fields every Video reads). `scale` is **canvas-relative**: `scale = 1.0` means the Clip fills the canvas at the source's aspect ratio; the renderer up- or down-scales the source as needed. Position `{x, y}` is in canvas coordinates with `(0, 0)` at top-left.

### Audio gain

`audioGain` is stored and surfaced in **dB**, from `−∞` (mute) to `+12 dB`, with `0 dB` as the recorded level.

### Transcripts move to per-Segment, derived at read

`transcript.json` shape becomes `{ schemaVersion: 2, segmentTranscripts: [ { segmentId, words[…words in source time] }, … ] }`. Mapping to the Video timeline is a derivation: for each word, find the Clip(s) referencing its Segment, compute `videoTime = wordStartInSource − clip.inPoint + clip.startOnTimeline`, drop words that fall inside Cut spans. The derivation function is shared with `core::compose` (same Clip → Video time math).

Only the Scene's marked **Transcript Source** Segments are transcribed.

### Lazy on-read migration for v1 Courses

v1 Course Folders are read by v2 builds without rewriting them on disk:

- `course.json` — `frameSize` is optional on read, defaulted to `1920×1080 30fps` when missing; written only on the next `course.json` mutation. `schemaVersion: 1` stays valid.
- `edits.json` v1 logs (cuts only) replay under v2 as the implicit single-Track / single-Clip case. The implicit Track / Clip are synthesized from the Video's lone Segment for the duration of the in-memory session; they're materialised to disk only when the user mutates (which by definition is a v2 action — e.g. adding a Track, moving a Clip, recording a multi-source Take).
- Per-Segment sidecars — missing sidecars are synthesized at read as `{ sourceRole: screen, takeId: <derived per-Segment>, recordedAt: <file mtime>, defaults: <Source-Role default for screen> }`. Written to disk only on mutation.
- `transcript.json` v1 (flat word list) wraps as a single `segmentTranscripts` entry keyed to the Video's only Segment.

A v1 Course Folder copied back to a v1 build still works until the first v2 mutation; after that, the folder's files are v2 and v1 builds can no longer open it.

## Why

- **Tracks first-class** because per-Track metadata (lock, solo, mute, name, kind) needs a home now or immediately after — every NLE the user might compare us to has them, and the refactor cost is the same whether we pay it now or in three sprints when a feature forces it.
- **Video-level ripple cuts** because course creators want a cut to drop "that ten-second moment" from screen + camera + mic at once. Per-Track cuts force them to recut N times for the same intent and risk leaving the sources out of alignment. Ripple (rather than lift) because the alternative — leaving a hole that every downstream Clip has to manually be slid past — is the same N-times-recut problem in a different costume.
- **Linked Clips by `takeId`** because ADR-0002 paid heavily for frame-accurate sync at Capture, and that investment evaporates the first time a user nudges one Track in the editor. Linking by default protects the invariant; explicit Unlink retains the user's freedom when they actually want it.
- **Command log** because we already have it, it gives unlimited free undo, it round-trips v1's existing logs cleanly, and it scales for the volumes here (tens of Clips × tens of property changes per Video). A snapshot-with-side-undo-stack would rebuild plumbing we already have.
- **One shared compose model** because divergence between preview-via-CSS and export-via-ffmpeg is a class of bug we'd find only at export time, with users noticing the layout shifted. The math is small enough that doing it once is cheaper than a snapshot-comparison harness.
- **Per-Course frame size** because solo course creators produce a whole Course at one aspect ratio. A per-Video setting is more UI for a case that's vanishingly rare in courses.
- **Canvas-relative `scale`** matches user intuition ("the camera is 25% size = 25% of the canvas") and means a Scene's `defaults` block doesn't need to know each device's native resolution. Source-relative scale (Resolve's "Zoom") would force every Scene author to think about pixel math.
- **Audio gain in dB** is the convention every pro tool uses; a `0..1` slider would be the unconventional choice and would force a conversion at export anyway.
- **Per-Segment transcripts** make Assembly changes (moving Clips, splitting Takes, recording a new Take) free — none of them require re-transcribing. Storing words in *Video-timeline* time would force a transcript rewrite on every nudge.
- **Lazy migration** keeps the local-first / portable-folder promise of ADR-0001: a v1 Course Folder can be read by a v2 build and copied back to a v1 build untouched. Eager rewrite-on-open would break that round-trip.

## Consequences

- **The EDL is a public contract from v2 onwards.** Adding new command variants in future versions needs the same `schemaVersion`-gated replay logic the v1 → v2 migration introduces.
- **`core::compose` is the canonical contract.** Any visual or audio property that exists in the preview must exist in the export and vice versa. Adding a property (e.g. rotation, blur) is two-renderer work plus one compose-model change.
- **Auto-arrangement on Keep ([Behaviour A — append per Role onto matching Tracks, align to end of Video timeline]).** When a Take is Kept, its N Segments are placed onto Tracks matching their Source Role (creating Tracks as needed), at the time-position equal to the end of the Video's assembled timeline. The Scene's `defaults` are copied into the new Clips as their initial `position`, `scale`, `opacity`, `audioGain`.
- **No Media Bin in v1.5.** Captured Segments are auto-placed Clips on the timeline; there is no separate library view. A Media Bin re-emerges as useful when the editor accepts externally-imported media, which is a v2 concern.
- **No `lift` cut affordance.** Cuts are always ripple. Adding `lift` later would be a new command variant (`AddLiftCut`) and a corresponding compose-math branch.
- **The exporter rewrites significantly.** Today's `trim + concat` filter graph becomes `overlay + amix` driven by the compose layout description. `default_export_dir` and the cancel/progress contract are unchanged. The trim+concat path is removed; v2 has only the composite render path, and v1 single-source Videos render through it as the trivial one-Track / one-Clip case.
- **Transcript schema is versioned alongside edits.** `transcript.json` gains its own `schemaVersion`, 1 → 2, and the same lazy-on-read migration policy.
- **The schema migration is asymmetric.** v2 reads v1 fine; v1 cannot read v2. Users on v2 must take care if they copy folders to colleagues still on v1 — a "this folder was edited on a newer Courseforge" warning may eventually be worth surfacing in v1 builds.
- **Per-Course Project Settings live on `course.json`.** This is the first non-skeleton-ish field on `course.json`. Reads of v1 files stay backward-compatible (optional + default); writes after v2 mutation include the field. The `schemaVersion` on `course.json` remains `1` because the addition is strictly additive and optional; a future ADR can bump it if a *breaking* change arrives.

## Considered and rejected

- **Per-Track cuts (a cut applies only to one Track at a time).** Rejected: forces the user to make the same cut N times across the N Tracks of one Take to drop a moment they said wrong — exactly the toil video-level cuts exist to remove. The few cases where you genuinely want to cut one Track only (mute just the screen recording for a few seconds) are better served by per-Clip `visible: false` ranges, which is a v2 feature.
- **Snapshot EDL with separate undo stack.** Rejected: rebuilds an undo subsystem the v1 command log already gives us for free. The size argument (snapshot is bigger on disk) is also against it — for a multi-track Video, the snapshot is much bigger than the command log per save.
- **Two independent compositors verified by snapshot tests.** Rejected: snapshot harnesses for video are fragile (codec/font/driver drift in CI) and the divergence-finding loop is "user exports, sees their layout shifted, files a bug" — too slow. One pure compose model eliminates the class of bug.
- **Per-Video frame size and FPS.** Rejected for v1.5: more UI for a case (Videos in one Course with different aspect ratios) that's vanishingly rare in courses. Re-considerable when Courses mix talking-head 16:9 with phone-portrait 9:16, which we don't see in our user.
- **Source-relative `scale` semantics (Resolve's "Zoom" default).** Rejected: forces every Scene author and every renderer codepath to know the source's native resolution. Canvas-relative is the simpler invariant.
- **Eager migration on first open of a v1 Course.** Rejected: breaks the v1↔v2 folder portability promise; concentrates the migration risk into one large atomic write on a folder the user hasn't necessarily committed to upgrading.
- **Refuse to open v1 Course Folders.** Rejected: hostile to existing users and undermines ADR-0001's "the filesystem is the source of truth".
- **Storing transcripts in Video-timeline time.** Rejected: every Assembly change (moving a Clip, splitting a Take, recording a new Take) would force a transcript rewrite. Per-Segment in source time + derivation-at-read keeps the data minimal.
- **Linking Clips by `segmentId` rather than `takeId`.** Rejected: `segmentId` links a Clip only to other Clips referencing the *same media file* — useful for "this same camera angle used twice", but doesn't capture the cross-source sync that's the actual invariant we want to preserve. `takeId` does.
