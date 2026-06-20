use serde::Deserialize;
use std::path::PathBuf;
use serde::Serialize;

#[derive(Deserialize, Default)]
pub struct UserConfig {
    pub display: Option<DisplayConfig>,
}

#[derive(Deserialize)]
pub struct DisplayConfig {
    pub grouping: Option<u8>,
}

pub fn load_config() -> Option<UserConfig> {
    let config_path = config_path()?;
    let content = std::fs::read_to_string(&config_path).ok()?;
    toml::from_str(&content).ok()
}

pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    Some(PathBuf::from(home).join(".hexwife.toml"))
}



#[derive(Serialize)]
struct SaveConfig {
    display: SaveDisplay,
}

#[derive(Serialize)]
struct SaveDisplay {
    grouping: u8,
}

pub fn save_config(grouping: u8) {
    let path = match config_path() {
        Some(p) => p,
        None => return,
    };
    let cfg = SaveConfig {
        display: SaveDisplay { grouping },
    };
    if let Ok(toml_str) = toml::to_string_pretty(&cfg) {
        let _ = std::fs::write(&path, toml_str);
    }
}