use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct Settings {
    pub folder: Option<String>,
    pub last_file: Option<String>,
    pub last_page: Option<u32>,
    /// Name stored as the author of comments you add. Defaults to your Windows user name.
    pub author: Option<String>,
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
