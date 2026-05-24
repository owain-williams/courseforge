# Filesystem as source of truth for Courses

## Context

Courseforge needs to know which Courses exist, where their state lives, and how to persist changes. The product values "local-first" and "portable artifacts" — a Course is meant to be a folder the user can copy, zip, move between Macs, and have it just work. We had to decide how the Library discovers Courses, how a Course Folder is laid out on disk, and how destructive operations behave.

## Decision

**The user's filesystem is the system of record for Courses. Courseforge does not maintain authoritative state outside it, and does not try to be clever about coordination or destruction.**

Concretely:

### Library

The Library is reconstructed from disk on every scan. There is no separate registry that can drift out of sync with what's on disk. Sources:

- A **Scanned Root** — a single user-chosen directory, picked once on first launch (default `~/Courseforge/`). Recursive scan; descent stops when a `course.json` is found (the file acts as a package sentinel, so users can group Courses into subdirs without us reading inside them).
- **Pinned Folders** — Course Folders living outside the Scanned Root that the user has explicitly added, surfaced as "Add Existing Course…" in the UI.

Scans run on launch, on window focus, after any in-app create/rename/delete, and on manual refresh. No live file watching in v1.

### Course Folder layout

```
<course-folder>/
  course.json          # skeleton: title, modules[], videos[] (opaque IDs, ordered)
  videos/
    <video-id>/        # opaque-ID-named, flat (not nested under modules)
      segments/        # raw recordings (MKV)
      transcript.json
      edits.json
      …
```

- `course.json` is the only marker that makes a directory a Course Folder.
- Module membership lives only in `course.json`'s arrays — moving a Video between Modules is a JSON edit, not a directory move.
- `course.json#title` is the canonical title for display. The Course Folder's own name is a human-readable slug, kept best-effort in sync on in-app rename, but allowed to drift (e.g., if the user renames the folder in Finder, the JSON title still wins).
- All paths inside `course.json` are relative to the Course Folder. Copying the folder to another Mac produces a working Course with no import step.
- `schemaVersion: 1` from day one to make future migrations tractable.

### Deletion

Deletion is always via macOS Trash; there is no hard-delete path in the app.

At Course level, this splits into two operations because the intents are genuinely different:
- **Remove from Library** — forgets a Course (unpins a Pinned Folder, or removes a Scanned-Root entry from the Library view) without touching the bytes.
- **Move to Trash** — sends the Course Folder to the macOS Trash.

At Module and Video level, delete is a single confirmed cascade to Trash. There is no "soft remove" that leaves orphan files outside the course's awareness.

### Coordination

No cross-process lockfile in v1. Within one Courseforge process, the same Course Folder is allowed in at most one window ("already open — bring window forward?"). The exotic case of two Macs editing the same folder over a network share is documented as "don't do this" rather than engineered around.

## Why

A registry, a database, or a lockfile would each move some authority *off* the filesystem and *into* Courseforge — and each would create a class of bugs where the app's view of the world disagrees with what's actually on disk. Those bugs are corrosive to a local-first product: users who move folders in Finder, restore from Time Machine, sync across machines via Dropbox, or copy a folder to a friend would all be punished for trusting the "portable artifacts" promise.

By making the filesystem authoritative, we make those workflows free. The cost is that we give up some power (we can't enforce coordination across machines, can't index Courses without a scan, can't have rich query behavior across the Library). For v1 that cost is small and the trust gain is large.

## Consequences

- The on-disk format is a public contract from v1 onwards. Breaking changes require schema migration.
- Library queries beyond "list courses with title + counts" require either a scan-time aggregation pass or a per-Course in-memory index. We accept this — v1's Library view is intentionally minimal (title, mtime, counts; no thumbnails, no workflow summaries).
- We rely on the OS for last-modified information (folder mtime) rather than maintaining our own field. This is a deliberate trade — slight inaccuracy from things like Spotlight or Finder touches, in exchange for not having to remember to bump a timestamp on every write.
- We rely on the macOS Trash for recovery from accidental deletes. There is no in-app undo for "deleted a Course."
- Two Courseforge instances on different Macs editing the same Course Folder via a sync service will last-writer-wins on `course.json`. Documented as unsupported.

## Considered and rejected

- **Hidden registry (`~/Library/Application Support/Courseforge/library.json`)** — rejected: drifts out of sync with disk; punishes users who move folders.
- **SQLite at the Course Folder root** — rejected for the skeleton: opaque to the user, undermines the "open the folder and see what's there" feel. Not ruled out for downstream per-Video state (transcripts, edit decisions) where the access pattern argues for it.
- **Module-nested per-Video subfolders (`modules/<id>/videos/<id>/…`)** — rejected: makes cross-module move expensive and racy for no benefit.
- **Folder names as canonical titles** — rejected: forces folder renames on every title edit, creates collision-handling complexity, and breaks if the user renames in Finder.
- **Cross-process lockfile** — rejected for v1: lock-leak after crash, network-volume edge cases, force-open UX, and a vanishingly small set of real users hitting the problem. Reconsider if it actually bites.
