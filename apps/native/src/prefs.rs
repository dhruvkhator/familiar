//! This app's own preferences (appearance, reduce motion), kept in `~/.familiar/native-ui.json`. Command-line flags
//! win over the file for that launch.

use familiar_ui::AppearanceMode;
use serde::{Deserialize, Serialize};

use crate::engine::familiar_home;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub theme: AppearanceMode,
    pub reduce_motion: bool,
}

fn path() -> std::path::PathBuf {
    familiar_home().join("native-ui.json")
}

pub fn load() -> Prefs {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Apply `change` to the stored preferences and write them back (best effort).
pub fn update(change: impl FnOnce(&mut Prefs)) {
    let mut p = load();
    change(&mut p);
    let _ = std::fs::create_dir_all(familiar_home());
    if let Ok(json) = serde_json::to_vec_pretty(&p) {
        let _ = std::fs::write(path(), json);
    }
}
