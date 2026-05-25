import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

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
export type Video = { id: string; title: string; stateId: string | null };
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
  sources: CaptureSources;
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
  sources?: CaptureSources
) =>
  invoke<SessionSnapshot>('start_recording', {
    folder,
    videoId,
    sources: sources ?? null
  });

export const pauseRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('pause_recording', { sessionId });

export const resumeRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('resume_recording', { sessionId });

export const stopRecording = (sessionId: string) =>
  invoke<SessionSnapshot>('stop_recording', { sessionId });

export const keepSegment = (sessionId: string) =>
  invoke<Segment>('keep_segment', { sessionId });

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
