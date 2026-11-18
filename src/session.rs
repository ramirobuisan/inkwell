//! What survives a crash: which project and page were open, whether the last run ended
//! cleanly, and the newest unsaved project state, which a panic writes out before the
//! program dies.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use crate::project;

#[derive(Clone, Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Session {
    pub last_project: Option<PathBuf>,
    pub page: usize,
    pub clean_exit: bool,
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("inkwell")
}

fn session_path() -> PathBuf {
    data_dir().join("session.json")
}

impl Session {
    /// The saved session; a first run counts as clean.
    pub fn load() -> Self {
        match fs::read(session_path()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Session {
                clean_exit: true,
                ..Default::default()
            },
        }
    }

    pub fn save(&self) {
        let _ = fs::create_dir_all(data_dir());
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = project::atomic_write(&session_path(), &json);
        }
    }
}

/// The project state not yet on disk: (project.json path, contents).
static PENDING: Mutex<Option<(PathBuf, String)>> = Mutex::new(None);

pub fn set_pending(dir: &Path, json: String) {
    if let Ok(mut g) = PENDING.lock() {
        *g = Some((dir.join(project::FILE), json));
    }
}

pub fn clear_pending() {
    if let Ok(mut g) = PENDING.lock() {
        *g = None;
    }
}

/// On a panic, write what wasn't saved yet and a short crash note, then carry on with the
/// usual panic report.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Ok(mut g) = PENDING.try_lock() {
            if let Some((path, json)) = g.take() {
                let _ = project::atomic_write(&path, json.as_bytes());
            }
        }
        let _ = fs::create_dir_all(data_dir());
        let _ = fs::write(data_dir().join("crash.log"), format!("{info}\n"));
        default(info);
    }));
}
