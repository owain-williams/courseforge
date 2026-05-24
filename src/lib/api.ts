import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

export type AppConfig = {
  scannedRoot: string | null;
};

export type CourseEntry = {
  folder: string;
  title: string;
  modified_ms: number;
  video_count: number;
};

export const getConfig = () => invoke<AppConfig>('get_config');

export const setScannedRoot = (root: string) =>
  invoke<AppConfig>('set_scanned_root', { root });

export const defaultScannedRoot = () =>
  invoke<string | null>('default_scanned_root');

export const createCourse = (root: string, title: string) =>
  invoke<string>('create_course', { root, title });

export const scanLibrary = (root: string) =>
  invoke<CourseEntry[]>('scan_library', { root });

export const renameCourse = (folder: string, newTitle: string) =>
  invoke<string>('rename_course', { folder, newTitle });

export const pickDirectory = async (defaultPath?: string | null) => {
  const result = await open({
    directory: true,
    multiple: false,
    defaultPath: defaultPath ?? undefined,
    title: 'Choose a folder to scan for Courses'
  });
  return typeof result === 'string' ? result : null;
};
