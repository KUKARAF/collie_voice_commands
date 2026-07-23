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

const SUMMARY_SYSTEM_PROMPT: &str = "You summarize what a terminal-based AI coding agent just \
    did, for someone who will only hear this read aloud and can't see the screen. Be concise \
    (2-4 sentences), plain language, no markdown, no code blocks — describe outcomes, not raw \
    output.";

const SUPERVISOR_SUMMARY_SYSTEM_PROMPT: &str = "You summarize what happened across one or more \
    terminal panes, each running an AI coding agent, for someone who will only hear this read \
    aloud and can't see the screen. Name each pane you mention. Be concise (a sentence or two \
    per pane), plain language, no markdown, no code blocks — describe outcomes, not raw output.";

#[derive(Debug, Deserialize)]
struct ResolvedReply {
    reply: String,
    #[serde(default)]
    keys: Option<Vec<String>>,
}

/// The outcome of resolving + sending one instruction to one pane. Shared between a direct
/// single-pane `send_command` and the supervisor's per-target dispatch loop.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneDispatchResult {
    pub pane_id: String,
    /// Filled in by callers that have a display name handy (the supervisor does, from the
    /// snapshot it already fetched); `null` for the single-pane `send_command` path, where the
    /// frontend already knows the pane it's talking to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_name: Option<String>,
    /// "text" | "keys" — which endpoint actually carried the resolved reply.
    pub sent_mode: String,
    /// The literal text typed, or the keys joined with a space, for display.
    pub sent_content: String,
    /// The pane's tail *before* sending — what the agent was showing when the operator spoke,
    /// so the turn-detail UI can show what a keys-based resolution was reacting to.
    pub pre_send_context: String,
    /// The pane's tail *after* the agent finished, ahead of summarization.
    pub raw_output: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendCommandResult {
    pub summary: String,
    pub audio_base64: String,
    pub dispatch: PaneDispatchResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorResult {
    pub summary: String,
    pub audio_base64: String,
    /// Empty when the supervisor answered directly from fleet status without touching any pane.
    pub dispatches: Vec<PaneDispatchResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SupervisorDecision {
    action: String, // "dispatch" | "answer"
    #[serde(default)]
    targets: Option<Vec<SupervisorTarget>>,
    #[serde(default)]
    answer: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SupervisorTarget {
    pane_id: String,
    instruction: String,
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

    let dispatch =
        dispatch_to_pane(&collie, &openrouter, &settings.reply_model, &pane_id, &text).await?;
    let summary =
        summarize_output(&openrouter, &settings.summarize_model, &dispatch.raw_output).await?;
    let audio_base64 = synthesize(&openrouter, &settings, &summary).await?;

    Ok(SendCommandResult {
        summary,
        audio_base64,
        dispatch,
    })
}

/// Fleet-wide command: decides which pane(s) an instruction is for (or answers directly from
/// fleet status with no pane contact at all) instead of always targeting one pre-selected pane.
#[tauri::command]
pub async fn send_supervisor_command(
    app: AppHandle,
    text: String,
) -> Result<SupervisorResult, String> {
    let settings = settings::load(&app)?;
    if settings.openrouter_api_key.trim().is_empty() {
        return Err("set an OpenRouter API key in Settings first".into());
    }

    let collie = CollieClient::new(settings.collie_base_url.clone());
    let openrouter = OpenRouterClient::new(settings.openrouter_api_key.clone());

    let snapshot = collie.snapshot().await.map_err(|e| e.to_string())?;
    let listing = fleet_listing(&snapshot);

    let decision = supervisor_decide(&openrouter, &settings.reply_model, &listing, &text).await?;

    if decision.action == "answer" {
        let answer = decision
            .answer
            .unwrap_or_else(|| "no status available".into());
        let audio_base64 = synthesize(&openrouter, &settings, &answer).await?;
        return Ok(SupervisorResult {
            summary: answer,
            audio_base64,
            dispatches: vec![],
        });
    }

    let targets = decision.targets.unwrap_or_default();
    if targets.is_empty() {
        return Err("supervisor didn't choose any pane to act on".into());
    }

    let mut dispatches = Vec::with_capacity(targets.len());
    for target in targets {
        let pane_name = find_pane_name(&snapshot, &target.pane_id);
        let result = dispatch_to_pane(
            &collie,
            &openrouter,
            &settings.reply_model,
            &target.pane_id,
            &target.instruction,
        )
        .await?;
        dispatches.push(PaneDispatchResult {
            pane_name: Some(pane_name),
            ..result
        });
    }

    let summary = summarize_dispatches(&openrouter, &settings.summarize_model, &dispatches).await?;
    let audio_base64 = synthesize(&openrouter, &settings, &summary).await?;

    Ok(SupervisorResult {
        summary,
        audio_base64,
        dispatches,
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
    synthesize(&openrouter, &settings, &text).await
}

/// Resolve → send → wait → read for one pane. Shared by `send_command` (single pre-selected
/// pane) and `send_supervisor_command`'s per-target loop (multiple panes chosen by the LLM).
async fn dispatch_to_pane(
    collie: &CollieClient,
    openrouter: &OpenRouterClient,
    reply_model: &str,
    pane_id: &str,
    instruction: &str,
) -> Result<PaneDispatchResult, String> {
    let tail = collie
        .read_pane(pane_id, Some(200))
        .await
        .map_err(|e| e.to_string())?
        .text;
    let pre_send_context = truncate_tail(&tail);

    let resolved = resolve_reply(openrouter, reply_model, &tail, instruction).await?;

    let (sent_mode, sent_content) = match resolved.keys.filter(|k| !k.is_empty()) {
        Some(keys) => {
            let joined = keys.join(" ");
            let action = collie
                .keys(pane_id, keys)
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
                .reply(pane_id, &resolved.reply)
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

    wait_for_pane_idle(collie, pane_id).await?;

    let final_text = collie
        .read_pane(pane_id, Some(200))
        .await
        .map_err(|e| e.to_string())?
        .text;

    Ok(PaneDispatchResult {
        pane_id: pane_id.to_string(),
        pane_name: None,
        sent_mode,
        sent_content,
        pre_send_context,
        raw_output: truncate_tail(&final_text),
    })
}

async fn summarize_output(
    openrouter: &OpenRouterClient,
    model: &str,
    raw_output: &str,
) -> Result<String, String> {
    openrouter
        .chat_text(model, SUMMARY_SYSTEM_PROMPT, raw_output)
        .await
        .map_err(|e| e.to_string())
}

async fn summarize_dispatches(
    openrouter: &OpenRouterClient,
    model: &str,
    dispatches: &[PaneDispatchResult],
) -> Result<String, String> {
    let mut combined = String::new();
    for d in dispatches {
        let name = d.pane_name.as_deref().unwrap_or(&d.pane_id);
        combined.push_str(&format!(
            "### {name}\ninstruction: {}\noutput:\n{}\n\n",
            d.sent_content, d.raw_output
        ));
    }
    openrouter
        .chat_text(model, SUPERVISOR_SUMMARY_SYSTEM_PROMPT, &combined)
        .await
        .map_err(|e| e.to_string())
}

async fn synthesize(
    openrouter: &OpenRouterClient,
    settings: &Settings,
    text: &str,
) -> Result<String, String> {
    let audio = openrouter
        .tts(
            &settings.tts_model,
            &settings.tts_voice,
            text,
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

/// One line per pane, fed to the supervisor's routing call as its only knowledge of the fleet.
fn fleet_listing(snapshot: &SnapshotResponse) -> String {
    snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .map(|p| {
            let label = p
                .pane_label
                .clone()
                .or_else(|| p.session_name.clone())
                .unwrap_or_else(|| p.agent.clone());
            format!(
                "- paneId={} agent={} label={} workspace={} status={} cwd={}",
                p.pane_id,
                p.agent,
                label,
                p.workspace_label,
                p.status.as_str(),
                p.cwd
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn find_pane_name(snapshot: &SnapshotResponse, pane_id: &str) -> String {
    snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .find(|p| p.pane_id == pane_id)
        .map(|p| {
            let label = p
                .pane_label
                .clone()
                .or_else(|| p.session_name.clone())
                .unwrap_or_else(|| p.agent.clone());
            format!("{} · {label}", p.agent)
        })
        .unwrap_or_else(|| pane_id.to_string())
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

/// Decides whether an operator instruction can be answered from fleet status alone, or should
/// be dispatched to one or more specific panes.
async fn supervisor_decide(
    openrouter: &OpenRouterClient,
    model: &str,
    fleet_listing: &str,
    user_text: &str,
) -> Result<SupervisorDecision, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["dispatch", "answer"] },
            "targets": {
                "type": ["array", "null"],
                "items": {
                    "type": "object",
                    "properties": {
                        "paneId": { "type": "string" },
                        "instruction": { "type": "string" }
                    },
                    "required": ["paneId", "instruction"],
                    "additionalProperties": false
                }
            },
            "answer": { "type": ["string", "null"] }
        },
        "required": ["action", "targets", "answer"],
        "additionalProperties": false
    });
    let system = "You are a supervisor across a fleet of terminal panes, each possibly running \
        an AI coding agent. You're given a listing of every pane (id, agent, label, workspace, \
        status, cwd) and an operator instruction. If the instruction can be answered directly \
        from the fleet status shown (e.g. \"what's blocked\", \"how many are working\", \
        \"what's running in the billing workspace\"), respond with action \"answer\" and put \
        the answer in `answer`, leaving `targets` null — do not contact any pane for a pure \
        status question. If the instruction should be carried out on one or more specific \
        panes (referred to by name, label, workspace, or clearly implied), respond with action \
        \"dispatch\" and list each target pane's id plus an instruction phrased for that pane, \
        leaving `answer` null. Only target panes that actually exist in the listing.";
    let user = format!("Fleet:\n{fleet_listing}\n\nOperator said: {user_text}");
    let value = openrouter
        .chat_json(model, system, &user, "collie_supervisor", schema)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| format!("supervisor decision response: {e}"))
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
