pub mod core;
mod commands;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::get_config,
            commands::set_scanned_root,
            commands::default_scanned_root,
            commands::create_course,
            commands::scan_library,
            commands::rename_course,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
