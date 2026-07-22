use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::time::{sleep, Duration};

use crate::collie::{
    find_focused_pane, AgentStatus, CollieClient, PaneReadResponse, SnapshotResponse,
};
use crate::openrouter::OpenRouterClient;
use crate::settings::{self, Settings};

const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const POLL_TIMEOUT_ITERATIONS: u32 = 60; // ~90s — no stronger "done" signal exists than status.
const CONTEXT_CHARS: usize = 4000; // pre-send/raw-output context kept for the turn-detail UI.

#[derive(Debug, Deserialize)]
struct ResolvedReply {
    reply: String,
    #[serde(default)]
    keys: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendCommandResult {
    pub summary: String,
    pub audio_base64: String,
    pub pane_id: String,
    /// "text" | "keys" — which endpoint actually carried the resolved reply.
    pub sent_mode: String,
    /// The literal text typed, or the keys joined with a space, for display.
    pub sent_content: String,
    /// The pane's tail *before* sending — what the agent was showing when the operator
    /// spoke, so the turn-detail UI can show what a keys-based resolution was reacting to.
    pub pre_send_context: String,
    /// The pane's tail *after* the agent finished, ahead of summarization.
    pub raw_output: String,
}

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<Settings, String> {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(app: AppHandle, new_settings: Settings) -> Result<(), String> {
    settings::save(&app, &new_settings)
}

#[tauri::command]
pub async fn send_command(
    app: AppHandle,
    text: String,
    pane_id: Option<String>,
) -> Result<SendCommandResult, String> {
    let settings = settings::load(&app)?;
    if settings.openrouter_api_key.trim().is_empty() {
        return Err("set an OpenRouter API key in Settings first".into());
    }

    let collie = CollieClient::new(settings.collie_base_url.clone());
    let openrouter = OpenRouterClient::new(settings.openrouter_api_key.clone());

    let pane_id = match pane_id {
        Some(id) => id,
        None => {
            let snapshot = collie.snapshot().await.map_err(|e| e.to_string())?;
            find_focused_pane(&snapshot)
                .ok_or("no focused pane — open one in the Collie UI first, or pick one from Fleet")?
                .pane_id
                .clone()
        }
    };

    let tail = collie
        .read_pane(&pane_id, Some(200))
        .await
        .map_err(|e| e.to_string())?
        .text;
    let pre_send_context = truncate_tail(&tail);

    let resolved = resolve_reply(&openrouter, &settings.reply_model, &tail, &text).await?;

    let (sent_mode, sent_content) = match resolved.keys.filter(|k| !k.is_empty()) {
        Some(keys) => {
            let joined = keys.join(" ");
            let action = collie
                .keys(&pane_id, keys)
                .await
                .map_err(|e| e.to_string())?;
            if !action.ok {
                return Err(action
                    .error
                    .unwrap_or_else(|| "collie rejected the action".into()));
            }
            ("keys".to_string(), joined)
        }
        None => {
            let action = collie
                .reply(&pane_id, &resolved.reply)
                .await
                .map_err(|e| e.to_string())?;
            if !action.ok {
                return Err(action
                    .error
                    .unwrap_or_else(|| "collie rejected the action".into()));
            }
            ("text".to_string(), resolved.reply)
        }
    };

    wait_for_pane_idle(&collie, &pane_id).await?;

    let final_text = collie
        .read_pane(&pane_id, Some(200))
        .await
        .map_err(|e| e.to_string())?
        .text;

    let summary = openrouter
        .chat_text(
            &settings.summarize_model,
            "You summarize what a terminal-based AI coding agent just did, for someone who \
             will only hear this read aloud and can't see the screen. Be concise (2-4 \
             sentences), plain language, no markdown, no code blocks — describe outcomes, not \
             raw output.",
            &final_text,
        )
        .await
        .map_err(|e| e.to_string())?;

    let audio = openrouter
        .tts(
            &settings.tts_model,
            &settings.tts_voice,
            &summary,
            &settings.tts_format,
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(SendCommandResult {
        summary,
        audio_base64: STANDARD.encode(audio),
        pane_id,
        sent_mode,
        sent_content,
        pre_send_context,
        raw_output: truncate_tail(&final_text),
    })
}

#[tauri::command]
pub async fn get_snapshot(app: AppHandle) -> Result<SnapshotResponse, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie.snapshot().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn read_pane(
    app: AppHandle,
    pane_id: String,
    lines: Option<u32>,
) -> Result<PaneReadResponse, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .read_pane(&pane_id, lines)
        .await
        .map_err(|e| e.to_string())
}

/// Synthesizes speech for arbitrary text (e.g. the "pane needs you" attention alert) without
/// running the full send_command pipeline.
#[tauri::command]
pub async fn speak(app: AppHandle, text: String) -> Result<String, String> {
    let settings = settings::load(&app)?;
    if settings.openrouter_api_key.trim().is_empty() {
        return Err("set an OpenRouter API key in Settings first".into());
    }
    let openrouter = OpenRouterClient::new(settings.openrouter_api_key.clone());
    let audio = openrouter
        .tts(
            &settings.tts_model,
            &settings.tts_voice,
            &text,
            &settings.tts_format,
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(STANDARD.encode(audio))
}

fn truncate_tail(text: &str) -> String {
    if text.len() <= CONTEXT_CHARS {
        return text.to_string();
    }
    let start = text.len() - CONTEXT_CHARS;
    // Don't split a UTF-8 char in half.
    let start = (start..text.len())
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(text.len());
    format!("…{}", &text[start..])
}

/// Turns loose operator input into the literal thing to send to the pane — typed text, or a
/// raw key sequence when the pane is clearly waiting on a menu/confirmation. See
/// project_scope.md "Agentic reply resolution".
async fn resolve_reply(
    openrouter: &OpenRouterClient,
    model: &str,
    pane_tail: &str,
    user_text: &str,
) -> Result<ResolvedReply, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "reply": { "type": "string" },
            "keys": {
                "type": ["array", "null"],
                "items": { "type": "string" }
            }
        },
        "required": ["reply", "keys"],
        "additionalProperties": false
    });
    let system = "You control a terminal pane running an AI coding agent. You're given the \
        pane's recent output and an operator instruction. Decide what to literally send: \
        typed text to submit (`reply`), or a sequence of raw key names (`keys`) — only when \
        the pane is clearly waiting on a menu/selection/confirmation that maps to specific \
        keystrokes (e.g. arrow keys then Enter, a single digit, a single letter). Valid key \
        names: Enter, Tab, Escape, Up, Down, Left, Right, Backspace, or a single printable \
        character. Otherwise put the instruction in `reply` verbatim and leave `keys` null.";
    let user = format!("Pane output:\n{pane_tail}\n\nOperator said: {user_text}");
    let value = openrouter
        .chat_json(model, system, &user, "collie_reply", schema)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| format!("reply resolution response: {e}"))
}

async fn wait_for_pane_idle(collie: &CollieClient, pane_id: &str) -> Result<(), String> {
    for _ in 0..POLL_TIMEOUT_ITERATIONS {
        sleep(POLL_INTERVAL).await;
        let snapshot = collie.snapshot().await.map_err(|e| e.to_string())?;
        let status = snapshot
            .agents
            .iter()
            .chain(snapshot.shell_panes.iter())
            .find(|a| a.pane_id == pane_id)
            .map(|a| a.status.clone());
        match status {
            Some(AgentStatus::Working) => continue,
            Some(_) => return Ok(()),
            // Pane vanished (closed/renamed away) — nothing more to wait for.
            None => return Ok(()),
        }
    }
    Err("timed out waiting for the agent to finish — check the Collie UI".into())
}
