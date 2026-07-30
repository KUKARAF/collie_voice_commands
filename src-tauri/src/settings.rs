use serde::{Deserialize, Serialize};
use std::fs;
use tauri::{AppHandle, Manager};

/// This app's own local settings — now just "where's collie-server". All LLM/TTS/model
/// configuration used to live here too, but that logic has been ported server-side
/// (collie-server owns its own OpenRouter key via `kv` and its own model/TTS/speak-toggle
/// settings, reachable at `GET/PUT /api/settings` on collie-server — out of scope for this
/// app's UI to manage today).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub collie_base_url: String,
    /// How long a fired "pane needs you" notification waits for a response before the monitor
    /// escalates to speaking the question aloud via TTS. Long enough that a normal glance-and-
    /// respond doesn't false-escalate, short enough to matter for the walking/driving/hands-busy
    /// case this whole feature exists for. `#[serde(default)]` so existing on-disk
    /// `settings.json` files without this field (written before this setting existed) still
    /// parse instead of failing to load.
    #[serde(default = "default_notification_response_timeout_secs")]
    pub notification_response_timeout_secs: u32,
}

fn default_notification_response_timeout_secs() -> u32 {
    90
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            collie_base_url: "https://thinkpad.sparidae-chinstrap.ts.net".into(),
            notification_response_timeout_secs: default_notification_response_timeout_secs(),
        }
    }
}

fn settings_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("resolving app data dir: {e}"))?;
    fs::create_dir_all(&dir).map_err(|e| format!("creating app data dir: {e}"))?;
    Ok(dir.join("settings.json"))
}

pub fn load(app: &AppHandle) -> Result<Settings, String> {
    let path = settings_path(app)?;
    match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|e| format!("parsing settings.json: {e}"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(e) => Err(format!("reading settings.json: {e}")),
    }
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    let contents =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serializing settings: {e}"))?;
    fs::write(&path, contents).map_err(|e| format!("writing settings.json: {e}"))
}
