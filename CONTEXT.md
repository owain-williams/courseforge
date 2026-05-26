# Courseforge

A local-first macOS desktop app for solo course creators — ideate, record, edit, and export online video courses end to end. Single context.

## Language

### Workflow phases

A solo creator's work on a Video moves through four named phases. The UI surfaces them as distinct modes / views so the user always knows what they're doing.

**Planning**:
Shaping the Course skeleton — Modules, Videos, titles, workflow state. No media exists yet. The Course window's outline + workflow-state board lives here.
_Avoid_: Outlining, design.

**Capture**:
Producing Segments by recording. Includes [[Scene]] selection / authoring and the Source Picker. Ends when the user clicks Keep on an N-source Take.
_Avoid_: Shooting, recording (the act is a "recording session"; the phase is Capture).

**Assembly**:
Laying out captured Segments onto the Video's multi-track timeline. Auto-applied on Keep (the [[Scene]]'s layout decides which Track each Segment lands on, where in time, and with what initial `{position, scale, opacity}`). The user nudges from there.
_Avoid_: Layout, arrangement.

**Editing**:
Refining a Video that's already assembled — adding cuts (Video-level, drop spans across every Track), adjusting per-Clip properties, transcript-driven trimming, export. Cuts and edits never modify Segments on disk.
_Avoid_: Polish, post-production.

### Editor concepts

**Track**:
A persistent row on a Video's timeline that holds one or more [[Clip]]s. Tracks own ordering (`order`), `kind` (`video` | `audio`), and per-Track metadata (name, lock, solo, mute) — separate from the Clips that sit on them.
_Avoid_: Layer, channel, lane.

**Clip**:
A placement of a [[Segment]] on a [[Track]] — `{ trackId, segmentId, startOnTimeline, inPoint, outPoint, position, scale, opacity, audioGain, visible }`. The Segment file is immutable; the Clip's properties are project state. Multiple Clips can reference the same Segment (e.g. one source reused at two points on the timeline).
_Avoid_: Instance, placement.

**Linked Clips**:
Clips that share a [[Take]] id are *linked by default* — selecting or moving one moves all. The link is what preserves the frame-accurate sync captured at recording time; without it the user silently breaks lip-sync the first time they nudge a Track. The user can explicitly **Unlink** in the editor when they intend to drift the sources apart.
_Avoid_: Grouped, tied.

**Cut**:
A time range on the *Video timeline* that is dropped at render time, ripple-style — downstream Clips slide left to close the gap (no leftover hole on any Track). A Cut applies to every Clip overlapping its span. Persisted in the EDL command log; never mutates Segments.
_Avoid_: Lift (we don't keep the gap), edit, trim.

**Scale semantics**:
Clip `scale` is **canvas-relative**: `scale = 1.0` means the Clip fills the Video's output frame at the source's aspect ratio (the renderer upscales / downscales the source to fit). A camera at `scale = 0.25` occupies a quarter of the canvas regardless of whether the camera captured at 720p or 4K. Position `{x, y}` is in canvas coordinates with `(0, 0)` at the top-left of the output frame.
_Avoid_: Zoom (Resolve's term — different default — confusing here).

**Audio gain**:
Clip `audioGain` is in **dB**, ranging from −∞ (mute) to +12 dB. `0 dB` is the source's recorded level. UI surfaces a fader; persistence stores the dB value.
_Avoid_: Volume (ambiguous with player volume).

### The course hierarchy

**Course**:
The top-level unit of work. An ordered collection of Modules, persisted on disk as a single self-contained Course Folder.
_Avoid_: Project, workspace.

**Module**:
A named, ordered grouping of Videos within a Course (e.g., "Introduction", "Mechanics"). Two levels of hierarchy (Course → Module → Video) is fixed for v1; no sub-modules.
_Avoid_: Section, chapter, unit.

**Video**:
The exportable unit. A timeline composed of one or more Segments plus non-destructive edit and effect decisions. A Video lives in exactly one Module.
_Avoid_: Lesson (LMS-flavoured — we are a creation tool, not an LMS), clip, episode.

**Segment**:
A single source's contiguous recording — the resulting media file from one capture device or stream during one Record session. A Video contains one or more Segments; a multi-source Record press produces N peer Segments that share a [[Take]] id. Segments are immutable raw captures; editing happens via non-destructive decisions on top.
_Avoid_: Clip, recording (use "recording session" for the act of capturing, "Segment" for the resulting artifact).

**Take**:
A grouping of Segments captured together in one Record press — N Segments (one per [[Source Role]]) that share a `takeId` and a wall-clock start. A Take is *derived* from per-Segment sidecars, not stored as its own directory; the grouping exists so the editor can lay the N children out aligned on the timeline by default.
_Avoid_: Session (reserved for the in-memory `RecordingSession` state machine), Clip, Recording.

**Scene**:
A named, reusable preset that defines what to record during one [[Capture]] press — an ordered set of [[Source Role]]s, each bound to a specific device, plus that source's initial `{position, scale, opacity}` on the Video frame. Scenes are course-level (one Scene used by many Videos), stored in a sibling `scenes.json` next to `course.json`. A Video can pin a default Scene; Capture uses that, or the user picks via "Record with…". When a Take is Kept, the Scene's layout is what auto-arranges the N Segments onto Tracks (the start of [[Assembly]]).
_Avoid_: Preset, layout, template.

**Source Role**:
What a Segment is a recording *of*. The taxonomy is closed at five values:
- `screen` — a whole display.
- `window` — a specific app window.
- `camera` — a video `AVCaptureDevice` (built-in, USB, Continuity Camera, etc.).
- `microphone` — an audio `AVCaptureDevice`.
- `system_audio` — what the OS is playing.

Each Segment carries one Source Role plus a `device { id, label }` so multiple-of-the-same-role in one Take (two cameras, two screens, mic + system audio) stay distinguishable in the UI.
_Avoid_: Capture source (was the name of the all-in-one boolean trio in v1; retired with multi-source recording).

### Persistence

**Course Folder**:
The on-disk directory that contains a Course. Self-contained — no external references, no machine-specific absolute paths. Copying a Course Folder to another Mac running Courseforge reproduces the Course faithfully. The folder is named with a human-readable slug of the Course title; `course.json` is the canonical source of truth for the title.
_Avoid_: Project folder, course directory.

**`course.json`**:
The marker file at the root of every Course Folder. Contains the Course skeleton — title, ordered Modules, ordered Videos, per-Video workflow state. Per-Video downstream state (transcripts, edit decisions, effects, exports) lives in per-Video subfolders alongside, not inside `course.json`.

**Library**:
The set of Courses Courseforge knows about. Sourced from the Scanned Root plus any Pinned Folders. The library is reconstructed from disk; there is no separate registry that can drift out of sync with what's actually present.
_Avoid_: Workspace, vault.

**Scanned Root**:
The single user-chosen directory Courseforge auto-scans for Course Folders. Any folder inside it containing a `course.json` is treated as a Course and appears in the Library.

**Pinned Folder**:
A Course Folder living outside the Scanned Root that the user has explicitly added to the Library — e.g., a Course Folder copied in from another Mac and kept in its original location. The user-facing affordance is "Add Existing Course…"; "Pin" is the internal term.

### Library operations

**Remove from Library**:
Removes a Course from the Library without touching its files. For a Pinned Folder, unpins it. For a Course in the Scanned Root, only available alongside Move to Trash as a deliberate "I want to forget about this but keep the bytes" option.

**Move to Trash**:
Sends the Course Folder to the macOS Trash and removes it from the Library. The only path to data deletion from inside the app — there is no hard-delete.

**Title drift**:
The situation where a Course Folder's name on disk differs from its `course.json#title`. Permitted and expected (e.g., after the user renames the folder in Finder). The JSON title always wins for display; the folder name is a Finder convenience, not authoritative.

## Example dialogue

> **Dev:** When the user reorders Videos within a Module, do we rename the on-disk folders?
> **Owner:** No — Video IDs are opaque. The per-Video subfolder name never changes. The order is just an array in `course.json`. The Course Folder itself is the only thing on disk named after a human title.
> **Dev:** And if I copy a Course Folder from another Mac into a directory that isn't the Scanned Root?
> **Owner:** It won't show up in the Library automatically. You'd Pin it — that registers its path without moving it. Pinned Folders and Scanned Root entries look identical in the Library view.
