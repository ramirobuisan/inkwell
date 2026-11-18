//! Preferences, with defaults picked for the machine the program runs on.

use std::{fs, path::PathBuf};

use serde::{Deserialize, Serialize};

use crate::project;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Where every dropped comic gets its own folder.
    pub library: PathBuf,
    pub ocr_lang: String,
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub target_lang: String,
    pub instructions: String,
    pub workers: usize,
    pub texture_cap: u32,
    pub page_cache: usize,
    pub autosave_ms: u64,
    pub default_font: String,
}

/// What this computer has: cores and memory.
pub struct Machine {
    pub cores: usize,
    pub memory_gb: f32,
}

impl Machine {
    pub fn detect() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let memory_gb = fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|kb| kb.parse::<f32>().ok())
            })
            .map(|kb| kb / 1024.0 / 1024.0)
            .unwrap_or(8.0);
        Self { cores, memory_gb }
    }
}

impl Default for Settings {
    fn default() -> Self {
        let m = Machine::detect();
        let big = m.memory_gb >= 12.0;
        let library = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(std::env::temp_dir)
            .join("Inkwell Library");
        Self {
            library,
            ocr_lang: "jpn".to_owned(),
            endpoint: "http://localhost:11434/v1".to_owned(),
            model: "qwen3:4b".to_owned(),
            api_key: String::new(),
            target_lang: "English".to_owned(),
            instructions: String::new(),
            workers: (m.cores / 2).max(2),
            texture_cap: if big { 4096 } else { 2048 },
            page_cache: if big { 6 } else { 4 },
            autosave_ms: 500,
            default_font: String::new(),
        }
    }
}

fn path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(crate::session::data_dir)
        .join("inkwell")
        .join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        fs::read(path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = project::atomic_write(&p, &json);
        }
    }
}
