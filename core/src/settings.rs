use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub struct Settings {
    pub folder: Option<String>,
    pub last_file: Option<String>,
    pub last_page: Option<u32>,
    /// Name stored as the author of comments you add. Defaults to your Windows user name.
    pub author: Option<String>,
    /// "openrouter" or "anthropic". API keys live in Windows Credential Manager, never in this file.
    pub ai_provider: Option<String>,
    pub ai_model_openrouter: Option<String>,
    pub ai_model_anthropic: Option<String>,
    /// Also search the other PDFs in the folder when answering.
    pub ai_use_folder: Option<bool>,

    /// Mouse-wheel scroll speed in the page view (1.0 = egui's default).
    pub scroll_speed: Option<f32>,
    /// "system", "dark" or "light".
    pub theme: Option<String>,
    /// Interface size, 1.0 = normal.
    pub ui_scale: Option<f32>,
    /// Reopen the tabs from last time on start (default true).
    pub reopen_tabs: Option<bool>,
    /// Look for a newer version on start, at most once a day (default true).
    pub auto_update: Option<bool>,
    /// Unix seconds of the last automatic update check.
    pub last_update_check: Option<u64>,
    /// The version whose "What's new" has already been shown.
    pub last_seen_version: Option<String>,
}

fn file(dir: &PathBuf) -> PathBuf {
    dir.join("settings.json")
}

pub fn load(dir: &PathBuf) -> Settings {
    std::fs::read_to_string(file(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(dir: &PathBuf, s: &Settings) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(file(dir), json).map_err(|e| e.to_string())
}
