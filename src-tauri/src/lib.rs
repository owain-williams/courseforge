pub mod core;
mod commands;
mod windows;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(windows::CourseWindowRegistry::default())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_scanned_root,
            commands::default_scanned_root,
            commands::create_course,
            commands::scan_library,
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
            commands::open_course_window,
            commands::get_window_course_folder,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
