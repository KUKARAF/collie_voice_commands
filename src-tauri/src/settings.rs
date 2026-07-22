use serde::{Deserialize, Serialize};
use std::fs;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub collie_base_url: String,
    pub openrouter_api_key: String,
    pub reply_model: String,
    pub summarize_model: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_format: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            collie_base_url: "https://thinkpad.sparidae-chinstrap.ts.net".into(),
            openrouter_api_key: String::new(),
            // Starting points only — verify against OpenRouter's live catalog/pricing and
            // change here in Settings whenever.
            reply_model: "openai/gpt-4o-mini".into(),
            summarize_model: "openai/gpt-4o-mini".into(),
            tts_model: "openai/gpt-4o-mini-tts".into(),
            tts_voice: "alloy".into(),
            tts_format: "mp3".into(),
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
