# Courseforge

A local-first macOS desktop app for solo course creators — ideate, record, edit, and export online video courses end to end. Single context.

## Language

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
A single contiguous recording. A Video contains one or more Segments arranged in order. Segments are immutable raw captures; editing happens via non-destructive decisions on top.
_Avoid_: Clip, take, recording (use "recording session" for the act of capturing, "Segment" for the resulting artifact).

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
