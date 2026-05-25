use std::path::PathBuf;
use serde::Serialize;
use tauri::{Manager, WebviewUrl, WebviewWindowBuilder};
use crate::core::{config, course, library, CoreError};
use crate::core::permissions::{
    self, CaptureSources, PermissionsSnapshot, SettingsPane,
};
use crate::core::segments::{self, OrphanSegment, Segment};
use crate::core::transcript::{self, Transcript};
use crate::recording_manager::{RecordingManager, SessionSnapshot};
use crate::transcription_manager::{TranscriptionJob, TranscriptionManager};
use crate::windows::{CourseWindowRegistry, OpenDecision};

#[derive(Debug, Serialize)]
pub struct AppError {
    message: String,
}

impl From<CoreError> for AppError {
    fn from(e: CoreError) -> Self {
        Self { message: e.to_string() }
    }
}

fn config_path<R: tauri::Runtime>(app: &tauri::AppHandle<R>) -> Result<PathBuf, AppError> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| AppError { message: format!("no app config dir: {e}") })?;
    std::fs::create_dir_all(&dir).map_err(|e| AppError {
        message: format!("failed to create config dir: {e}"),
    })?;
    Ok(dir.join(config::CONFIG_FILENAME))
}

#[tauri::command]
pub fn get_config(app: tauri::AppHandle) -> Result<config::AppConfig, AppError> {
    let path = config_path(&app)?;
    Ok(config::read_config(&path)?)
}

#[tauri::command]
pub fn set_scanned_root(app: tauri::AppHandle, root: PathBuf) -> Result<config::AppConfig, AppError> {
    let path = config_path(&app)?;
    let mut cfg = config::read_config(&path)?;
    cfg.scanned_root = Some(root);
    config::write_config(&path, &cfg)?;
    Ok(cfg)
}

#[tauri::command]
pub fn default_scanned_root() -> Option<PathBuf> {
    config::suggested_default_root()
}

#[tauri::command]
pub fn create_course(root: PathBuf, title: String) -> Result<PathBuf, AppError> {
    Ok(course::create_course(&root, &title)?)
}

#[tauri::command]
pub fn scan_library(root: PathBuf) -> Result<Vec<library::CourseEntry>, AppError> {
    Ok(library::scan_library(&root)?)
}

/// Build the merged Library view: Scanned-Root entries + Pinned Folders,
/// minus Ignored Folders. Reads the user's saved config so callers don't
/// have to pass it in.
#[tauri::command]
pub fn list_library(app: tauri::AppHandle) -> Result<Vec<library::CourseEntry>, AppError> {
    let cfg = config::read_config(&config_path(&app)?)?;
    Ok(library::library_view(&cfg)?)
}

/// "Add Existing Course…" — validate the folder is a Course Folder, pin it,
/// persist. Returns the updated config so the caller can reflect the new
/// pin set without an extra round trip.
#[tauri::command]
pub fn add_existing_course(
    app: tauri::AppHandle,
    folder: PathBuf,
) -> Result<config::AppConfig, AppError> {
    let path = config_path(&app)?;
    let mut cfg = config::read_config(&path)?;
    library::pin_folder(&mut cfg, &folder)?;
    config::write_config(&path, &cfg)?;
    Ok(cfg)
}

/// "Remove from Library" — for a Pinned Folder, unpin. For a Scanned-Root
/// entry, add to the ignored list so the next scan skips it. The folder on
/// disk is untouched (ADR-0001).
#[tauri::command]
pub fn remove_from_library(
    app: tauri::AppHandle,
    folder: PathBuf,
) -> Result<config::AppConfig, AppError> {
    let path = config_path(&app)?;
    let mut cfg = config::read_config(&path)?;
    let was_pinned = library::unpin_folder(&mut cfg, &folder);
    if !was_pinned {
        library::ignore_folder(&mut cfg, &folder);
    }
    config::write_config(&path, &cfg)?;
    Ok(cfg)
}

/// "Move to Trash" — send the Course Folder to the macOS Trash and tidy up
/// any pin/ignore entries that referenced it.
#[tauri::command]
pub fn move_course_to_trash(
    app: tauri::AppHandle,
    folder: PathBuf,
) -> Result<config::AppConfig, AppError> {
    let path = config_path(&app)?;
    let mut cfg = config::read_config(&path)?;
    library::move_course_folder_to_trash(&folder)?;
    library::unpin_folder(&mut cfg, &folder);
    library::unignore_folder(&mut cfg, &folder);
    config::write_config(&path, &cfg)?;
    Ok(cfg)
}

#[tauri::command]
pub fn rename_course(folder: PathBuf, new_title: String) -> Result<PathBuf, AppError> {
    Ok(course::rename_course(&folder, &new_title)?)
}

#[tauri::command]
pub fn read_course(folder: PathBuf) -> Result<course::Course, AppError> {
    Ok(course::read_course(&folder)?)
}

#[tauri::command]
pub fn add_module(folder: PathBuf, title: String) -> Result<course::Module, AppError> {
    Ok(course::add_module(&folder, &title)?)
}

#[tauri::command]
pub fn rename_module(folder: PathBuf, module_id: String, new_title: String) -> Result<(), AppError> {
    Ok(course::rename_module(&folder, &module_id, &new_title)?)
}

#[tauri::command]
pub fn reorder_modules(folder: PathBuf, ordered_ids: Vec<String>) -> Result<(), AppError> {
    Ok(course::reorder_modules(&folder, &ordered_ids)?)
}

#[tauri::command]
pub fn delete_module(folder: PathBuf, module_id: String) -> Result<(), AppError> {
    Ok(course::delete_module(&folder, &module_id)?)
}

#[tauri::command]
pub fn add_video(folder: PathBuf, module_id: String, title: String) -> Result<course::Video, AppError> {
    Ok(course::add_video(&folder, &module_id, &title)?)
}

#[tauri::command]
pub fn rename_video(folder: PathBuf, video_id: String, new_title: String) -> Result<(), AppError> {
    Ok(course::rename_video(&folder, &video_id, &new_title)?)
}

#[tauri::command]
pub fn reorder_videos_in_module(
    folder: PathBuf,
    module_id: String,
    ordered_ids: Vec<String>,
) -> Result<(), AppError> {
    Ok(course::reorder_videos_in_module(&folder, &module_id, &ordered_ids)?)
}

#[tauri::command]
pub fn delete_video(folder: PathBuf, video_id: String) -> Result<(), AppError> {
    Ok(course::delete_video(&folder, &video_id)?)
}

#[tauri::command]
pub fn add_workflow_state(folder: PathBuf, name: String) -> Result<course::WorkflowState, AppError> {
    Ok(course::add_workflow_state(&folder, &name)?)
}

#[tauri::command]
pub fn rename_workflow_state(
    folder: PathBuf,
    state_id: String,
    new_name: String,
) -> Result<(), AppError> {
    Ok(course::rename_workflow_state(&folder, &state_id, &new_name)?)
}

#[tauri::command]
pub fn reorder_workflow_states(folder: PathBuf, ordered_ids: Vec<String>) -> Result<(), AppError> {
    Ok(course::reorder_workflow_states(&folder, &ordered_ids)?)
}

#[tauri::command]
pub fn remove_workflow_state(
    folder: PathBuf,
    state_id: String,
    fallback_state_id: String,
) -> Result<(), AppError> {
    Ok(course::remove_workflow_state(&folder, &state_id, &fallback_state_id)?)
}

#[tauri::command]
pub fn set_video_state(folder: PathBuf, video_id: String, state_id: String) -> Result<(), AppError> {
    Ok(course::set_video_state(&folder, &video_id, &state_id)?)
}

#[tauri::command]
pub fn move_video_to_module(
    folder: PathBuf,
    video_id: String,
    target_module_id: String,
    index: usize,
) -> Result<(), AppError> {
    Ok(course::move_video_to_module(
        &folder,
        &video_id,
        &target_module_id,
        index,
    )?)
}

#[tauri::command]
pub fn open_course_window(
    app: tauri::AppHandle,
    registry: tauri::State<'_, CourseWindowRegistry>,
    folder: PathBuf,
) -> Result<(), AppError> {
    // Read the course up front so we can surface "not a course folder" as a
    // user-visible error, and to put the title in the window chrome.
    let course = course::read_course(&folder)?;

    let decision = registry.open_or_focus(&folder);
    match decision {
        OpenDecision::Focus { label } => {
            if let Some(win) = app.get_webview_window(&label) {
                let _ = win.set_focus();
            }
            Ok(())
        }
        OpenDecision::Spawn { label } => {
            let win = WebviewWindowBuilder::new(&app, &label, WebviewUrl::App("course/".into()))
                .title(format!("{} — Courseforge", course.title))
                .inner_size(1000.0, 720.0)
                .min_inner_size(700.0, 480.0)
                // Tauri's OS-level file-drop interceptor swallows HTML5
                // drag events before the webview sees them; turn it off so
                // the kanban board's native drag-and-drop works.
                .disable_drag_drop_handler()
                .build()
                .map_err(|e| AppError { message: format!("failed to open course window: {e}") })?;

            // Release the slot on close so the user can re-open the same Course.
            let app_handle = app.clone();
            let folder_for_close = folder.clone();
            win.on_window_event(move |event| {
                if let tauri::WindowEvent::Destroyed = event {
                    if let Some(reg) = app_handle.try_state::<CourseWindowRegistry>() {
                        reg.release(&folder_for_close);
                    }
                }
            });
            Ok(())
        }
    }
}

#[tauri::command]
pub fn get_window_course_folder(
    window: tauri::Window,
    registry: tauri::State<'_, CourseWindowRegistry>,
) -> Option<PathBuf> {
    registry.folder_for_label(window.label())
}

// ---------------------------------------------------------------------------
// Recording flow
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn recording_preflight() -> PermissionsSnapshot {
    let checker = permissions::default_checker();
    PermissionsSnapshot::capture(checker.as_ref())
}

/// Deep-link the user to the relevant Privacy & Security pane. We use macOS'
/// `open` so the system-handler URL is followed by the OS — no Tauri shell
/// plugin required.
#[tauri::command]
pub fn open_settings_pane(pane: SettingsPane) -> Result<(), AppError> {
    let url = pane.url();
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("/usr/bin/open")
            .arg(url)
            .status()
            .map_err(|e| AppError { message: format!("failed to open settings: {e}") })?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        // No-op on non-mac — the UI shouldn't be offering the link there.
        let _ = url;
    }
    Ok(())
}

#[tauri::command]
pub fn start_recording(
    manager: tauri::State<'_, RecordingManager>,
    folder: PathBuf,
    video_id: String,
    sources: Option<CaptureSources>,
) -> Result<SessionSnapshot, AppError> {
    let sources = sources.unwrap_or_default();
    Ok(manager.start_session(&folder, &video_id, sources)?)
}

#[tauri::command]
pub fn pause_recording(
    manager: tauri::State<'_, RecordingManager>,
    session_id: String,
) -> Result<SessionSnapshot, AppError> {
    Ok(manager.pause_session(&session_id)?)
}

#[tauri::command]
pub fn resume_recording(
    manager: tauri::State<'_, RecordingManager>,
    session_id: String,
) -> Result<SessionSnapshot, AppError> {
    Ok(manager.resume_session(&session_id)?)
}

#[tauri::command]
pub fn stop_recording(
    manager: tauri::State<'_, RecordingManager>,
    session_id: String,
) -> Result<SessionSnapshot, AppError> {
    Ok(manager.stop_session(&session_id)?)
}

#[tauri::command]
pub fn keep_segment(
    manager: tauri::State<'_, RecordingManager>,
    transcription: tauri::State<'_, TranscriptionManager>,
    session_id: String,
) -> Result<Segment, AppError> {
    // Snapshot the session so we know which Course/Video to transcribe
    // *before* keep_session evicts it from the registry.
    let sessions = manager.list_sessions();
    let snap = sessions.into_iter().find(|s| s.id == session_id);

    let seg = manager.keep_session(&session_id)?;

    if let Some(snap) = snap {
        transcription.enqueue(snap.course_folder, snap.video_id);
    }
    Ok(seg)
}

#[tauri::command]
pub fn discard_segment(
    manager: tauri::State<'_, RecordingManager>,
    session_id: String,
) -> Result<(), AppError> {
    Ok(manager.discard_session(&session_id)?)
}

#[tauri::command]
pub fn list_active_sessions(
    manager: tauri::State<'_, RecordingManager>,
) -> Vec<SessionSnapshot> {
    manager.list_sessions()
}

#[tauri::command]
pub fn has_active_recording(manager: tauri::State<'_, RecordingManager>) -> bool {
    manager.has_active_sessions()
}

#[tauri::command]
pub fn list_segments(folder: PathBuf, video_id: String) -> Result<Vec<Segment>, AppError> {
    Ok(segments::list_segments(&folder, &video_id)?)
}

#[tauri::command]
pub fn scan_orphan_segments(folder: PathBuf) -> Result<Vec<OrphanSegment>, AppError> {
    Ok(segments::scan_orphans(&folder)?)
}

/// Adopt an orphan `.partial.mkv` as a finished Segment (Keep equivalent for
/// crash-recovered files). Goes through the same remux step as `keep`, so
/// the resulting `.mp4` plays in the WebKit `<video>` element.
#[tauri::command]
pub fn import_orphan_segment(
    manager: tauri::State<'_, RecordingManager>,
    folder: PathBuf,
    video_id: String,
    segment_id: String,
) -> Result<Segment, AppError> {
    Ok(manager.adopt_orphan(&folder, &video_id, &segment_id)?)
}

/// Delete an orphan `.partial.mkv` (Discard equivalent for crash-recovered files).
#[tauri::command]
pub fn discard_orphan_segment(
    folder: PathBuf,
    video_id: String,
    segment_id: String,
) -> Result<(), AppError> {
    Ok(segments::discard_partial(&folder, &video_id, &segment_id)?)
}

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_transcription_jobs(
    transcription: tauri::State<'_, TranscriptionManager>,
) -> Vec<TranscriptionJob> {
    transcription.jobs()
}

#[tauri::command]
pub fn retry_transcription(
    transcription: tauri::State<'_, TranscriptionManager>,
    video_id: String,
) -> Result<(), AppError> {
    Ok(transcription.retry(&video_id)?)
}

#[tauri::command]
pub fn get_transcript(folder: PathBuf, video_id: String) -> Result<Option<Transcript>, AppError> {
    Ok(transcript::read_transcript(&folder, &video_id)?)
}
