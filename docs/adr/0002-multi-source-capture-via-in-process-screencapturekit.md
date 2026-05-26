# Multi-source capture via in-process ScreenCaptureKit + AVCaptureSession

## Context

The recorder today captures a single screen + (optionally) a single microphone into one `ffmpeg` child process which writes a single `.partial.mkv`, later remuxed to `<segment-id>.mp4`. A Segment is 1:1 with a media file, and `CaptureSources` is a fixed `{microphone, system_audio, webcam}` boolean trio designed for one bundled output.

Courseforge is adding multi-source recording: in one Capture press the user records any number of `screen`, `window`, `camera`, `microphone`, and `system_audio` sources, each to its own file, so the editor can arrange them as separable Tracks. This forces three intertwined decisions:

1. **Which macOS capture APIs we use**, since `ffmpeg`'s `avfoundation` input can't capture individual app windows — window capture needs ScreenCaptureKit (SCK).
2. **Where the capture pipeline runs** (N ffmpeg children, an out-of-process Swift sidecar, or in-process Rust↔ObjC bindings).
3. **What sync guarantee we offer across sources** — drift between a presenter's mouth and their voice is the kind of error the editor can't fix later.

The decision shapes everything downstream — the storage layout for a Take, the Source Picker UX (Scenes), the Pause/Resume semantics, and the orphan-recovery story.

## Decision

**Replace the `ffmpeg`-child capture backend with an in-process Rust subsystem that uses [`objc2`][objc2] bindings to AVFoundation and ScreenCaptureKit, writes per-source `AVAssetWriter` outputs, and guarantees frame-accurate cross-source sync via the shared `CMSampleBuffer` PTS clock.**

Concretely:

### Capture taxonomy

[[Source Role]] is closed at five values — `screen`, `window`, `camera`, `microphone`, `system_audio` — and a Take may contain multiple Segments of the same role (two cameras, two screens). Each Segment's sidecar carries `{ takeId, sourceRole, device { id, label }, recordedAt, defaults }`.

### Capture mechanism

- `screen`, `window`, `system_audio` → `SCStream` (one stream per source, all on the shared SCK clock).
- `camera`, `microphone` → `AVCaptureSession` device inputs piped to the same shared `AVAssetWriter` clock.
- One `AVAssetWriter` per Segment writes a `.mov`/`.mp4` directly. The `<id>.partial.mkv → <id>.mp4` remux step that existed as crash scaffolding goes away; AVAssetWriter's incremental fragment writes are robust to abrupt termination on their own terms.

### Lifecycle

- **Start is atomic.** All N writers initialise in parallel. If any fails (device missing, permission denied, SCK content filter rejection), the whole Take aborts and partials are cleaned up.
- **Pause / Resume / Stop are atomic across all N.** A global PTS marker tells every writer when to stop appending; on Resume new samples land at a contiguous PTS.
- **Mid-recording per-source failure drops that source, keeps the rest going.** A USB camera unplug at minute 32 of a 45-minute take ends that camera's Segment cleanly; the screen + mic Segments contain the full Take. The failed source's sidecar carries `endedReason: sourceFailed` with the failure PTS.
- **Keep / Discard is Take-level.** One decision applies to all N Segments; the per-source Keep affordance is deferred (the user resolves it in the editor by deleting unwanted Tracks).

### The Capture-side API

`CaptureSources` retires. The new capture request is `Vec<CaptureRequest>` where each entry is `{ role, deviceId, label, defaults }`. The `RecorderBackend` trait grows to accept this list and return a `RecordingSession` whose state machine still has the same shape (`Idle → Recording ⇄ Paused → AwaitingDecision → Persisted|Discarded`), now representing the *Take* rather than a single source. Per-source health is internal to the backend; the session state is at the Take level.

### Scenes

A [[Scene]] — a named, reusable preset binding Source Roles to specific devices plus the initial editor properties for each — is persisted in a sibling `scenes.json` at the Course Folder root (next to `course.json`). A Video can pin a default Scene. Hitting Capture uses that Scene; "Record with…" opens a picker. The Scene's `defaults` block is snapshotted into each Segment's sidecar at Capture time so a Segment continues to render the way it was authored even if the Scene is later edited.

A Scene also marks one Source as the **Transcript Source** — only that source's audio is transcribed.

### Orphan recovery

`scan_orphans` groups partial AssetWriter outputs by `takeId`. The UI offers Import / Discard *per Take*, not per file — adopting a Take pulls all its surviving Segments into the Video at once and auto-arranges them per the Scene's layout (captured in the sidecar).

## Why

- **Window capture is non-negotiable** (the user listed "individual windows" as the first thing they want). Window capture on modern macOS is ScreenCaptureKit; ffmpeg's `avfoundation` input cannot do it. Once we're using SCK for windows, doing the rest of the capture surface through SCK + AVCaptureSession gives one consistent clock for everything.
- **Frame-accurate sync requires a shared clock.** Independent processes drift by tens of milliseconds. With N ffmpeg children, that drift accumulates over a long Take and is unfixable in the editor — you'd be guessing at frame offsets per source per Take. Putting everything on the system's `CMTime` clock at capture means sync is the recorder's problem (solved by the OS) instead of the editor's problem (impossible).
- **AVAssetWriter eliminates the remux step.** Today `keep_session` calls `finalize_segment` which calls a remuxer (ffmpeg-based on macOS) to copy `.partial.mkv` → `.mp4` so WebKit's `<video>` element can play it. AVAssetWriter writes a `.mov` directly; WebKit plays `.mov` natively. The whole `Remuxer` trait can retire.
- **In-process is the right packaging choice for this codebase.** A separate Swift sidecar would need its own build, sign, notarise, and Tauri-sidecar bundling configuration. We already link CoreGraphics directly from Rust (`CGPreflightScreenCaptureAccess` in `core::permissions`); extending to SCK + AVCaptureSession via `objc2` keeps the binary count at one and the build pipeline unchanged.
- **Atomic Start, drop-one-continue mid-recording, Take-level Keep/Discard** match the user's mental model: "I'm doing a take with my whole setup; if my whole setup isn't ready, fix it and retry; if one piece dies mid-take, don't punish me by torching the rest; when I'm done, the take is one decision". Per-source granularity exists in pro NLEs but at a UX cost we don't need to pay in v1.5.

## Consequences

- **The on-disk format grows new files.** Per-Segment sidecars at `videos/<vid>/segments/<sid>.json` and `scenes.json` at the Course Folder root are part of the public on-disk contract from this change forward.
- **Camera and Microphone TCC bridging becomes mandatory.** The current `MacPermissionChecker` returns `Granted` optimistically for Camera and Microphone — that worked when one shared ffmpeg child surfaced a permission error at start. Multi-source needs to fail Start with a precise "Camera permission denied for device X" rather than spinning up some sources and silently dropping others. We add real `AVCaptureDevice.authorizationStatus` queries via `objc2`.
- **Crash isolation changes.** A bug in the capture path used to crash an `ffmpeg` child — the Tauri app survived. In-process means a bug in capture crashes the whole app. We mitigate with: panic-safe wrappers around every `objc2` boundary, per-stream error isolation so one writer's failure can't take down the others, and AssetWriter's own fragment-writing robustness (a process crash leaves a `.mov` readable up to the last fragment).
- **The `partial.mkv → mp4` remux pipeline retires.** The `Remuxer` trait and `remuxer/` module are removed. `finalize_segment` becomes "rename / move the AssetWriter output into the canonical location and write its final sidecar".
- **The `RecorderBackend` trait signature changes.** `start` now takes `Vec<CaptureRequest>` (with takeId + per-source defaults) instead of `CaptureSources`, and returns a handle that supervises N concurrent capture streams rather than one ffmpeg child.
- **`ffmpeg` and `ffprobe` remain in the build** — the exporter still uses them for the composite render path (see ADR-0003) and `ffprobe` is still useful for reading durations of imported media. Only the *recorder* side stops using them.
- **The fake recorder backend** (`recorder/fake.rs`) needs to grow to model N streams + Take-level state. Tests that drive the state machine continue to work; new tests cover atomic start, drop-one-continue, and per-Take orphan grouping.
- **Frame-accurate sync at capture is only valuable if it survives editing.** ADR-0003 makes Clips sharing a `takeId` linked-by-default in the editor specifically to preserve this investment.

## Considered and rejected

- **N parallel `ffmpeg` children, one per source.** Rejected: ffmpeg's `avfoundation` input cannot capture individual app windows, which is a stated requirement; cross-process clock drift would force the user to nudge offsets per Take; and Pause/Resume across N processes via `SIGSTOP`/`SIGCONT` can't hit a single frame boundary.
- **Out-of-process Swift sidecar.** Rejected: adds a second language and a second binary to the build / sign / notarise / Tauri-bundle pipeline. The capture surface is small enough that the `objc2` overhead in Rust is less work than maintaining the sidecar's plumbing.
- **Hybrid — SCK in a Swift sidecar for screen/window/system-audio, ffmpeg children for cameras/mics.** Rejected: two backends double the bug surface (each with its own pause/resume/stop story) and the boundary between them needs cross-process sync glue we already decided we don't want.
- **Best-effort (±50ms) sync instead of frame-accurate.** Rejected: cuts that jump between screencast and presenter's face in a course are exactly where lip-sync matters; once a Take is recorded with drift baked in, the editor can't recover it.
- **Per-source Keep/Discard at the AwaitingDecision step.** Rejected for v1.5: resolvable in the editor by deleting unwanted Tracks; the Keep prompt stays a single decision.
- **Ad-hoc Source Picker without named Scenes** (the recommended design in earlier grilling). Rejected: a course creator records the same setup over and over for an entire Course; Scenes are how that's expressed cleanly. Per-Video "last used" is a tempting middle ground but loses on cross-Video reuse.

[objc2]: https://crates.io/crates/objc2
