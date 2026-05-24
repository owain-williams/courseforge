use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use crate::core::error::{CoreError, Result};

pub const CONFIG_FILENAME: &str = "config.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AppConfig {
    #[serde(rename = "scannedRoot")]
    pub scanned_root: Option<PathBuf>,
}

pub fn read_config(config_path: &Path) -> Result<AppConfig> {
    if !config_path.exists() {
        return Ok(AppConfig::default());
    }
    let bytes = std::fs::read(config_path).map_err(|e| CoreError::Io {
        path: config_path.to_path_buf(),
        source: e,
    })?;
    serde_json::from_slice(&bytes).map_err(|e| CoreError::InvalidCourseJson {
        path: config_path.to_path_buf(),
        source: e,
    })
}

pub fn write_config(config_path: &Path, config: &AppConfig) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| CoreError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let json = serde_json::to_string_pretty(config).map_err(|e| CoreError::InvalidCourseJson {
        path: config_path.to_path_buf(),
        source: e,
    })?;
    std::fs::write(config_path, json).map_err(|e| CoreError::Io {
        path: config_path.to_path_buf(),
        source: e,
    })
}

pub fn suggested_default_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Courseforge"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_config_returns_default_when_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = read_config(&dir.path().join("config.json")).unwrap();
        assert_eq!(cfg, AppConfig::default());
    }

    #[test]
    fn write_then_read_config_round_trips_scanned_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let original = AppConfig {
            scanned_root: Some(PathBuf::from("/Users/me/Courseforge")),
        };
        write_config(&path, &original).unwrap();
        let loaded = read_config(&path).unwrap();
        assert_eq!(loaded, original);
    }

    #[test]
    fn write_config_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/config.json");
        write_config(&path, &AppConfig::default()).unwrap();
        assert!(path.is_file());
    }
}
