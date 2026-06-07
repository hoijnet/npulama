use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CachedModel {
    pub alias: String,
    pub device: String,  // "CPU" | "GPU" | "NPU"
    pub is_cached: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub port: u16,
    pub bind_all: bool,
    pub tokens: Vec<String>,
    pub require_auth: bool,
    pub autostart: bool,
    pub preferred_model: Option<String>,
    /// Context window size in tokens. Default 4096, max 131072 (128K).
    pub context_size: u32,
    /// Last-known model catalog — shown immediately on startup before SDK loads.
    #[serde(default)]
    pub cached_catalog: Vec<CachedModel>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 11434,
            bind_all: false,
            tokens: vec![],
            require_auth: false,
            autostart: false,
            preferred_model: None,
            context_size: 4096,
            cached_catalog: vec![],
        }
    }
}

impl Config {
    fn config_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("npulama")
            .join("config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            std::fs::write(path, s).ok();
        }
    }

    pub fn bind_addr(&self) -> String {
        let host = if self.bind_all { "0.0.0.0" } else { "127.0.0.1" };
        format!("{}:{}", host, self.port)
    }

    pub fn proxy_url(&self) -> String {
        let host = if self.bind_all { "0.0.0.0" } else { "127.0.0.1" };
        format!("http://{}:{}", host, self.port)
    }
}
