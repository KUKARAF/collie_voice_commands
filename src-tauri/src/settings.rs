use serde::{Deserialize, Serialize};
use std::fs;
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub collie_base_url: String,
    /// The effective, ready-to-use OpenRouter key. Either pasted in directly, or auto-cached
    /// here after `ensure_openrouter_key` provisions one via kv_manager — callers that just
    /// need to make an OpenRouter request never need to know which.
    #[serde(default)]
    pub openrouter_api_key: String,
    pub reply_model: String,
    pub summarize_model: String,
    pub tts_model: String,
    pub tts_voice: String,
    pub tts_format: String,
    /// kv_manager (kv.osmosis.page) fields, only needed to auto-provision an OpenRouter key
    /// when `openrouter_api_key` is empty — see `commands::ensure_openrouter_key`.
    #[serde(default)]
    pub kv_manager_base_url: String,
    #[serde(default)]
    pub kv_manager_api_key: String,
    #[serde(default = "default_kv_manager_entry_key")]
    pub kv_manager_entry_key: String,
    /// Whether to speak each outcome category aloud at all — every turn still gets classified
    /// and summarized either way (for the transcript text), these only gate the TTS call.
    #[serde(default = "default_true")]
    pub speak_issue_reports: bool,
    #[serde(default = "default_true")]
    pub speak_success_reports: bool,
    #[serde(default = "default_true")]
    pub speak_decision_needed: bool,
    /// Hard cap enforced by instruction in the summarization prompt (not a JSON schema
    /// constraint — there's no way to make a model count words at the schema level).
    #[serde(default = "default_tts_max_words")]
    pub tts_max_words: u32,
}

fn default_kv_manager_entry_key() -> String {
    "openrouter_management_key".into()
}

fn default_true() -> bool {
    true
}

fn default_tts_max_words() -> u32 {
    40
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            collie_base_url: "https://thinkpad.sparidae-chinstrap.ts.net".into(),
            openrouter_api_key: String::new(),
            // Starting points only — verify against OpenRouter's live catalog/pricing and
            // change here in Settings whenever. MiniMax across the board by preference: M3 for
            // both the resolver/reasoning calls and summarization, speech-2.8-turbo for TTS.
            // "English_expressive_narrator" is one of MiniMax's own documented English preset
            // voice ids.
            reply_model: "minimax/minimax-m3".into(),
            summarize_model: "minimax/minimax-m3".into(),
            tts_model: "minimax/speech-2.8-turbo".into(),
            tts_voice: "English_expressive_narrator".into(),
            tts_format: "mp3".into(),
            kv_manager_base_url: String::new(),
            kv_manager_api_key: String::new(),
            kv_manager_entry_key: default_kv_manager_entry_key(),
            speak_issue_reports: true,
            speak_success_reports: true,
            speak_decision_needed: true,
            tts_max_words: default_tts_max_words(),
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

// Models that shipped as *the default* in an earlier version — self-heal installs that still
// have exactly one of these cached, rather than requiring a manual Settings edit for a value
// the user never deliberately chose. Anyone who has actually customized their model keeps it —
// this only fires when the cached value still matches a known-past-default exactly.
const RETIRED_TTS_MODEL: &str = "openai/gpt-4o-mini-tts"; // never existed on OpenRouter at all
const PREVIOUS_DEFAULT_SUMMARIZE_MODEL: &str = "openai/gpt-4o-mini";
const PREVIOUS_DEFAULT_TTS_MODEL: &str = "hexgrad/kokoro-82m";
const PREVIOUS_DEFAULT_TTS_VOICE: &str = "af_heart";

pub fn load(app: &AppHandle) -> Result<Settings, String> {
    let path = settings_path(app)?;
    let mut settings: Settings = match fs::read_to_string(&path) {
        Ok(contents) => {
            serde_json::from_str(&contents).map_err(|e| format!("parsing settings.json: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Settings::default()),
        Err(e) => return Err(format!("reading settings.json: {e}")),
    };

    let mut healed = false;
    if settings.tts_model == RETIRED_TTS_MODEL
        || (settings.tts_model == PREVIOUS_DEFAULT_TTS_MODEL
            && settings.tts_voice == PREVIOUS_DEFAULT_TTS_VOICE)
    {
        let defaults = Settings::default();
        settings.tts_model = defaults.tts_model;
        settings.tts_voice = defaults.tts_voice;
        healed = true;
    }
    if settings.summarize_model == PREVIOUS_DEFAULT_SUMMARIZE_MODEL {
        settings.summarize_model = Settings::default().summarize_model;
        healed = true;
    }
    if healed {
        save(app, &settings)?;
    }
    Ok(settings)
}

pub fn save(app: &AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    let contents =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serializing settings: {e}"))?;
    fs::write(&path, contents).map_err(|e| format!("writing settings.json: {e}"))
}
