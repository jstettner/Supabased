use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub server_url: Option<String>,
    pub ca_cert: Option<String>,
}

pub fn config_path() -> PathBuf {
    let config_dir = dirs::config_dir()
        .expect("could not determine config directory")
        .join("supabased");
    config_dir.join("config.json")
}

pub fn load_config() -> Config {
    let path = config_path();
    let Ok(contents) = fs::read_to_string(&path) else {
        return Config::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

pub fn save_config(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(config)?;
    fs::write(&path, json)?;
    Ok(())
}
