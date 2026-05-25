pub mod core;
mod commands;
pub mod recorder;
pub mod recording_manager;
mod windows;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(windows::CourseWindowRegistry::default())
        .manage(recording_manager::RecordingManager::new(recorder::default_backend()))
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_scanned_root,
            commands::default_scanned_root,
            commands::create_course,
            commands::scan_library,
            commands::list_library,
            commands::add_existing_course,
            commands::remove_from_library,
            commands::move_course_to_trash,
            commands::rename_course,
            commands::read_course,
            commands::add_module,
            commands::rename_module,
            commands::reorder_modules,
            commands::delete_module,
            commands::add_video,
            commands::rename_video,
            commands::reorder_videos_in_module,
            commands::delete_video,
            commands::move_video_to_module,
            commands::add_workflow_state,
            commands::rename_workflow_state,
            commands::reorder_workflow_states,
            commands::remove_workflow_state,
            commands::set_video_state,
            commands::open_course_window,
            commands::get_window_course_folder,
            commands::recording_preflight,
            commands::open_settings_pane,
            commands::start_recording,
            commands::pause_recording,
            commands::resume_recording,
            commands::stop_recording,
            commands::keep_segment,
            commands::discard_segment,
            commands::list_active_sessions,
            commands::has_active_recording,
            commands::list_segments,
            commands::scan_orphan_segments,
            commands::import_orphan_segment,
            commands::discard_orphan_segment,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
