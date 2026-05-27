import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

/**
 * Render any thrown value as a user-facing string. Tauri commands reject
 * with an `AppError` (`{ message: string }`), which `String(e)` would
 * render as the useless literal "[object Object]"; this pulls `.message`
 * out and falls back to sensible defaults for plain Errors, strings, etc.
 */
export function formatError(e: unknown): string {
  if (e == null) return 'Unknown error';
  if (typeof e === 'string') return e;
  if (e instanceof Error) return e.message;
  if (typeof e === 'object') {
    const maybeMessage = (e as { message?: unknown }).message;
    if (typeof maybeMessage === 'string') return maybeMessage;
    try {
      return JSON.stringify(e);
    } catch {
      return String(e);
    }
  }
  return String(e);
}

export type AppConfig = {
  scannedRoot: string | null;
  pinnedFolders: string[];
  ignoredFolders: string[];
};

export type CourseSource = 'scanned' | 'pinned';

export type CourseEntry = {
  folder: string;
  title: string;
  modified_ms: number;
  video_count: number;
  source: CourseSource;
  missing: boolean;
};

export const getConfig = () => invoke<AppConfig>('get_config');

export const setScannedRoot = (root: string) =>
  invoke<AppConfig>('set_scanned_root', { root });

export const defaultScannedRoot = () =>
  invoke<string | null>('default_scanned_root');

export const createCourse = (root: string, title: string) =>
  invoke<string>('create_course', { root, title });

export const listLibrary = () => invoke<CourseEntry[]>('list_library');

export const addExistingCourse = (folder: string) =>
  invoke<AppConfig>('add_existing_course', { folder });

export const removeFromLibrary = (folder: string) =>
  invoke<AppConfig>('remove_from_library', { folder });

export const moveCourseToTrash = (folder: string) =>
  invoke<AppConfig>('move_course_to_trash', { folder });

export const renameCourse = (folder: string, newTitle: string) =>
  invoke<string>('rename_course', { folder, newTitle });

export type Module = { id: string; title: string; videoIds: string[] };
export type Video = {
  id: string;
  title: string;
  stateId: string | null;
  /// Optional pinned default Scene (issue #35). Absent when the Video has
  /// no pin — Record then opens the Scene picker (or falls back to the
  /// legacy Screen+Mic synthetic request list when no Scenes exist at all).
  defaultSceneId?: string;
};
export type WorkflowState = { id: string; name: string };
export type Course = {
  schemaVersion: number;
  title: string;
  workflowStates: WorkflowState[];
  modules: Module[];
  videos: Video[];
};

export const readCourse = (folder: string) => invoke<Course>('read_course', { folder });

export const addModule = (folder: string, title: string) =>
  invoke<Module>('add_module', { folder, title });

export const renameModule = (folder: string, moduleId: string, newTitle: string) =>
  invoke<void>('rename_module', { folder, moduleId, newTitle });

export const reorderModules = (folder: string, orderedIds: string[]) =>
  invoke<void>('reorder_modules', { folder, orderedIds });

export const deleteModule = (folder: string, moduleId: string) =>
  invoke<void>('delete_module', { folder, moduleId });

export const addVideo = (folder: string, moduleId: string, title: string) =>
  invoke<Video>('add_video', { folder, moduleId, title });

export const renameVideo = (folder: string, videoId: string, newTitle: string) =>
  invoke<void>('rename_video', { folder, videoId, newTitle });

export const reorderVideosInModule = (folder: string, moduleId: string, orderedIds: string[]) =>
  invoke<void>('reorder_videos_in_module', { folder, moduleId, orderedIds });

export const deleteVideo = (folder: string, videoId: string) =>
  invoke<void>('delete_video', { folder, videoId });

export const moveVideoToModule = (
  folder: string,
  videoId: string,
  targetModuleId: string,
  index: number
) => invoke<void>('move_video_to_module', { folder, videoId, targetModuleId, index });

export const addWorkflowState = (folder: string, name: string) =>
  invoke<WorkflowState>('add_workflow_state', { folder, name });

export const renameWorkflowState = (folder: string, stateId: string, newName: string) =>
  invoke<void>('rename_workflow_state', { folder, stateId, newName });

export const reorderWorkflowStates = (folder: string, orderedIds: string[]) =>
  invoke<void>('reorder_workflow_states', { folder, orderedIds });

export const removeWorkflowState = (
  folder: string,
  stateId: string,
  fallbackStateId: string
) => invoke<void>('remove_workflow_state', { folder, stateId, fallbackStateId });

export const setVideoState = (folder: string, videoId: string, stateId: string) =>
  invoke<void>('set_video_state', { folder, videoId, stateId });

export const openCourseWindow = (folder: string) =>
  invoke<void>('open_course_window', { folder });

export const getWindowCourseFolder = () =>
  invoke<string | null>('get_window_course_folder');

export const pickDirectory = async (defaultPath?: string | null) => {
  const result = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath ?? undefined,
    title: 'Choose a folder to scan for Courses'
  });
  return typeof result === 'string' ? result : null;
};

export const pickExistingCourseFolder = async () => {
  const result = await open({
    directory: true,
    multiple: false,
    title: 'Pick a Course Folder to add to your Library'
  });
  return typeof result === 'string' ? result : null;
};

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

export type PermissionStatus = 'granted' | 'denied' | 'notDetermined' | 'restricted';

export type PermissionsSnapshot = {
  screenRecording: PermissionStatus;
  camera: PermissionStatus;
  microphone: PermissionStatus;
};

export type SettingsPane = 'screenRecording' | 'camera' | 'microphone';

/// The closed Source Role taxonomy from ADR-0002. Phase 1 only exercises
/// Screen + Microphone; Window / Camera / SystemAudio land in Phase 2.
export type SourceRole = 'screen' | 'window' | 'camera' | 'microphone' | 'systemAudio';

export type Device = {
  id: string;
  label: string;
};

export type Position = { x: number; y: number };

export type CompositionDefaults = {
  position: Position;
  scale: number;
  opacity: number;
  audioGainDb: number;
};

export type CaptureRequest = {
  role: SourceRole;
  device: Device;
  defaults: CompositionDefaults;
};

/// v1 boolean trio retained as a UI-side view model only — translated into
/// `CaptureRequest[]` before crossing the IPC boundary in `startRecording`.
export type CaptureSources = {
  microphone: boolean;
  systemAudio: boolean;
  webcam: boolean;
};

export type SessionState =
  | 'idle'
  | 'recording'
  | 'paused'
  | 'awaitingDecision'
  | 'persisted'
  | 'discarded';

export type SessionSnapshot = {
  id: string;
  videoId: string;
  segmentId: string;
  courseFolder: string;
  state: SessionState;
  takeId: string;
  requests: CaptureRequest[];
  recordedAt: string;
};

const defaultCompositionDefaults = (): CompositionDefaults => ({
  position: { x: 0, y: 0 },
  scale: 1,
  opacity: 1,
  audioGainDb: 0
});

/// Build the Phase 1 capture request list from the legacy boolean trio.
/// Screen is always present; mic is added if the user toggled it on.
/// systemAudio + webcam are tracked but produce no request yet (Phase 2).
export const captureRequestsFromSources = (s: CaptureSources): CaptureRequest[] => {
  const reqs: CaptureRequest[] = [
    {
      role: 'screen',
      device: { id: 'default', label: 'Main Display' },
      defaults: defaultCompositionDefaults()
    }
  ];
  if (s.microphone) {
    reqs.push({
      role: 'microphone',
      device: { id: 'default', label: 'Default Microphone' },
      defaults: defaultCompositionDefaults()
    });
  }
  return reqs;
};

export type Segment = {
  id: string;
  videoId: string;
  path: string;
};

export type OrphanSegment = Segment;

export const recordingPreflight = () =>
  invoke<PermissionsSnapshot>('recording_preflight');

export const openSettingsPane = (pane: SettingsPane) =>
  invoke<void>('open_settings_pane', { pane });

export const startRecording = (
  folder: string,
  videoId: string,
  requests: CaptureRequest[]
) =>
  invoke<SessionSnapshot>('start_recording', {
    folder,
    videoId,
    requests
  });

/// Issue #35 — Scene-driven recording. The backend reads the Scene,
/// validates each non-"default" device against the live device list, and
/// builds the `Vec<CaptureRequest>` itself. A missing device fails
/// pre-Start with a per-source diagnostic.
export const startRecordingWithScene = (
  folder: string,
  videoId: string,
  sceneId: string
) =>
  invoke<SessionSnapshot>('start_recording_with_scene', {
    folder,
    videoId,
    sceneId
  });

/// Pin (or unpin with `null`) the default Scene used by Record for one
/// Video. Persists in course.json so the choice survives a Course Folder
/// copy.
export const pinDefaultScene = (
  folder: string,
  videoId: string,
  sceneId: string | null
) =>
  invoke<void>('pin_default_scene', {
    folder,
    videoId,
    sceneId
  });

export const pauseRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('pause_recording', { sessionId });

export const resumeRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('resume_recording', { sessionId });

export const stopRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('stop_recording', { sessionId });

/// Issue #36 — Keep promotes every per-source partial in the Take to its
/// final name and writes a per-Segment sidecar each. Returns one Segment
/// record per slot so the UI can refresh its per-Video Segment list.
export const keepSegment = (sessionId: string) =>
  invoke<Segment[]>('keep_segment', { sessionId });

export const discardSegment = (sessionId: string) =>
  invoke<void>('discard_segment', { sessionId });

export const listActiveSessions = () =>
  invoke<SessionSnapshot[]>('list_active_sessions');

export const hasActiveRecording = () =>
  invoke<boolean>('has_active_recording');

export const listSegments = (folder: string, videoId: string) =>
  invoke<Segment[]>('list_segments', { folder, videoId });

export const scanOrphanSegments = (folder: string) =>
  invoke<OrphanSegment[]>('scan_orphan_segments', { folder });

export const importOrphanSegment = (folder: string, videoId: string, segmentId: string) =>
  invoke<Segment>('import_orphan_segment', { folder, videoId, segmentId });

export const discardOrphanSegment = (folder: string, videoId: string, segmentId: string) =>
  invoke<void>('discard_orphan_segment', { folder, videoId, segmentId });

// --- Per-Take orphan recovery (issue #38) ---

export type OrphanTakeSegment = {
  segmentId: string;
  sourceRole: SourceRole;
  partialPath: string;
};

export type OrphanTake = {
  takeId: string;
  videoId: string;
  /// ISO-8601 timestamp from the Take marker, or null for v1 legacy
  /// orphans that predate Take markers.
  recordedAt: string | null;
  sceneId: string | null;
  segments: OrphanTakeSegment[];
};

export const scanOrphanTakes = (folder: string) =>
  invoke<OrphanTake[]>('scan_orphan_takes', { folder });

export const importOrphanTake = (folder: string, videoId: string, takeId: string) =>
  invoke<Segment[]>('import_orphan_take', { folder, videoId, takeId });

export const discardOrphanTake = (folder: string, videoId: string, takeId: string) =>
  invoke<void>('discard_orphan_take', { folder, videoId, takeId });

// ---------------------------------------------------------------------------
// Scenes (Phase 2 — issue #33)
// ---------------------------------------------------------------------------

export type SceneSource = {
  role: SourceRole;
  device: Device;
  defaults: CompositionDefaults;
};

export type Scene = {
  id: string;
  name: string;
  sources: SceneSource[];
};

export const listScenes = (folder: string) =>
  invoke<Scene[]>('list_scenes', { folder });

export const createScene = (folder: string, name: string) =>
  invoke<Scene>('create_scene', { folder, name });

export const renameScene = (folder: string, sceneId: string, newName: string) =>
  invoke<void>('rename_scene', { folder, sceneId, newName });

export const duplicateScene = (folder: string, sceneId: string) =>
  invoke<Scene>('duplicate_scene', { folder, sceneId });

export const deleteScene = (folder: string, sceneId: string) =>
  invoke<void>('delete_scene', { folder, sceneId });

export const addSceneSource = (folder: string, sceneId: string, role: SourceRole) =>
  invoke<SceneSource>('add_scene_source', { folder, sceneId, role });

export const removeSceneSource = (folder: string, sceneId: string, sourceIndex: number) =>
  invoke<void>('remove_scene_source', { folder, sceneId, sourceIndex });

export const setSceneSourceDevice = (
  folder: string,
  sceneId: string,
  sourceIndex: number,
  device: Device
) =>
  invoke<SceneSource>('set_scene_source_device', {
    folder,
    sceneId,
    sourceIndex,
    device
  });

/// Persist composition defaults (position/scale/opacity/audioGainDb)
/// for one source row in a Scene. Issue #39 — debounced on the frontend.
export const setSceneSourceDefaults = (
  folder: string,
  sceneId: string,
  sourceIndex: number,
  defaults: CompositionDefaults
) =>
  invoke<SceneSource>('set_scene_source_defaults', {
    folder,
    sceneId,
    sourceIndex,
    defaults
  });

/// Move one source row to a different index inside its Scene. Row order
/// doubles as canvas z-order — later rows paint on top of earlier ones.
export const reorderSceneSource = (
  folder: string,
  sceneId: string,
  fromIndex: number,
  toIndex: number
) =>
  invoke<void>('reorder_scene_source', {
    folder,
    sceneId,
    fromIndex,
    toIndex
  });

/// Live device enumeration per role (issue #34). Queries SCK /
/// AVCaptureDevice at call time so USB / Continuity Camera changes show up
/// on Refresh. An empty list is not an error — the host just has no
/// hardware of that role attached.
export const listCaptureDevices = (role: SourceRole) =>
  invoke<Device[]>('list_capture_devices', { role });

/// The "use the system default" entry every dropdown carries — keeps
/// Device.id == "default" so Scenes stay portable across machines.
export const DEFAULT_DEVICE: Device = { id: 'default', label: 'Default' };

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

export type Word = { start: number; end: number; text: string };

export type Transcript = {
  schemaVersion: number;
  videoId: string;
  segmentIds: string[];
  words: Word[];
};

export type JobStatus =
  | { kind: 'pending' }
  | { kind: 'running'; fraction: number }
  | { kind: 'done' }
  | { kind: 'failed'; message: string };

export type TranscriptionJob = {
  videoId: string;
  courseFolder: string;
  status: JobStatus;
};

export const listTranscriptionJobs = () =>
  invoke<TranscriptionJob[]>('list_transcription_jobs');

export const retryTranscription = (videoId: string) =>
  invoke<void>('retry_transcription', { videoId });

export const getTranscript = (folder: string, videoId: string) =>
  invoke<Transcript | null>('get_transcript', { folder, videoId });

// ---------------------------------------------------------------------------
// Transcript-driven edits (EDL)
// ---------------------------------------------------------------------------

export type Cut = {
  id: string;
  startSec: number;
  endSec: number;
};

export type EditState = {
  cuts: Cut[];
  canUndo: boolean;
  canRedo: boolean;
};

export const getEditState = (folder: string, videoId: string) =>
  invoke<EditState>('get_edit_state', { folder, videoId });

export const addCut = (folder: string, videoId: string, startSec: number, endSec: number) =>
  invoke<EditState>('add_cut', { folder, videoId, startSec, endSec });

export const undoEdit = (folder: string, videoId: string) =>
  invoke<EditState>('undo_edit', { folder, videoId });

export const redoEdit = (folder: string, videoId: string) =>
  invoke<EditState>('redo_edit', { folder, videoId });

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export type ExportStatus =
  | { kind: 'pending' }
  | { kind: 'running'; fraction: number }
  | { kind: 'done'; mp4: string; srt: string }
  | { kind: 'failed'; message: string }
  | { kind: 'cancelled' };

export type ExportJob = {
  videoId: string;
  courseFolder: string;
  destinationDir: string;
  status: ExportStatus;
};

export const defaultExportDir = (folder: string, videoId: string) =>
  invoke<string>('default_export_dir', { folder, videoId });

export const startExport = (
  folder: string,
  videoId: string,
  destinationDir?: string | null
) =>
  invoke<ExportJob>('start_export', {
    folder,
    videoId,
    destinationDir: destinationDir ?? null
  });

export const cancelExport = (videoId: string) =>
  invoke<void>('cancel_export', { videoId });

export const listExportJobs = () => invoke<ExportJob[]>('list_export_jobs');

export const pickExportDirectory = async (defaultPath?: string | null) => {
  const result = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath ?? undefined,
    title: 'Choose where to save the exported MP4'
  });
  return typeof result === 'string' ? result : null;
};
