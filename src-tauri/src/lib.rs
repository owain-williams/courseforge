pub mod core;
mod commands;
pub mod recorder;
pub mod recording_manager;
pub mod transcriber;
pub mod transcription_manager;
mod windows;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(windows::CourseWindowRegistry::default())
        .manage(recording_manager::RecordingManager::new(recorder::default_backend()))
        .manage(transcription_manager::TranscriptionManager::new(
            transcriber::default_backend(),
        ))
        .setup(|app| {
            use tauri::{Emitter, Manager};
            // Bridge the manager's subscriber to a Tauri event so the
            // frontend can react. The worker thread drives `process_pending`
            // whenever the queue might have work.
            let handle = app.handle().clone();
            {
                let mgr = app.state::<transcription_manager::TranscriptionManager>();
                mgr.set_subscriber(Box::new(move |job| {
                    let _ = handle.emit("transcription-job", job);
                }));
            }

            let app_handle = app.handle().clone();
            std::thread::spawn(move || loop {
                let state =
                    app_handle.state::<transcription_manager::TranscriptionManager>();
                let n = state.process_pending();
                if n == 0 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
            Ok(())
        })
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
            commands::list_transcription_jobs,
            commands::retry_transcription,
            commands::get_transcript,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
