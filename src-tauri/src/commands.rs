use std::path::PathBuf;
use serde::Serialize;
use tauri::Manager;
use crate::core::{config, course, library, CoreError};

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

#[tauri::command]
pub fn rename_course(folder: PathBuf, new_title: String) -> Result<PathBuf, AppError> {
    Ok(course::rename_course(&folder, &new_title)?)
}
