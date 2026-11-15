//! Persisted client-side configuration: theme mode and saved environments
//! (server endpoints). Lives at `$C67_CONFIG_HOME/config.json`, defaulting to
//! `~/.config/c67/config.json`. Written through `ClientConfig::save()` after
//! any mutation so a restart restores the same environment + theme.

use serde::{Deserialize, Serialize};

/// A server the user can connect to (label + endpoint + bearer token).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SavedEnvironment {
    pub label: String,
    pub http_url: String,
    pub token: String,
}

/// Appearance mode: `"dark"`, `"light"`, or `"system"`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
    System,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default)]
    pub theme_mode: ThemeMode,
    /// Accent hue override reserved for future use; None = default accent.
    #[serde(default)]
    pub accent: Option<String>,
    #[serde(default)]
    pub environments: Vec<SavedEnvironment>,
    /// Index into `environments` of the last-connected endpoint.
    #[serde(default)]
    pub active_environment: Option<usize>,
}

fn config_path() -> Option<std::path::PathBuf> {
    if let Ok(dir) = std::env::var("C67_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(dir).join("config.json"));
    }
    let home = std::env::var("HOME").ok()?;
    Some(
        std::path::PathBuf::from(home)
            .join(".config")
            .join("c67")
            .join("config.json"),
    )
}

impl ClientConfig {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, text);
        }
    }
}
