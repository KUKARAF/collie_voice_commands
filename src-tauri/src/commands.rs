use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tokio::time::{sleep, Duration};

use crate::collie::{
    find_focused_pane, find_pane_by_id, pane_display_name, AgentStatus, CollieClient,
    PaneReadResponse, SnapshotResponse,
};
use crate::kvmanager::KvManagerClient;
use crate::openrouter::{self, OpenRouterClient};
use crate::settings::{self, Settings};
use crate::supervisor_tools;

const POLL_INTERVAL: Duration = Duration::from_millis(1500);
const POLL_TIMEOUT_ITERATIONS: u32 = 60; // ~90s — no stronger "done" signal exists than status.
const CONTEXT_CHARS: usize = 4000; // pre-send/raw-output context kept for the turn-detail UI.

#[derive(Debug, Deserialize)]
struct ResolvedReply {
    reply: String,
    #[serde(default)]
    keys: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
struct Classification {
    /// "success" | "issue" | "decision_needed" — which of Settings' three speak-toggles gates
    /// whether this actually gets synthesized to audio.
    category: String,
    summary: String,
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
    /// "success" | "issue" | "decision_needed" — see `Classification`.
    pub category: String,
    /// Empty string when the outcome's category is toggled off in Settings — classification and
    /// summary still happen either way (for the transcript text), only the TTS call is skipped.
    pub audio_base64: String,
    pub dispatch: PaneDispatchResult,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorResult {
    pub summary: String,
    /// "success" | "issue" | "decision_needed" for a dispatch; absent (empty string) for a
    /// direct fleet-status "answer", which isn't gated by the three speak-toggles.
    pub category: String,
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

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedOption {
    /// Button text.
    pub label: String,
    /// Free-form text — deliberately *not* pre-resolved to keys. Sent through the same
    /// `send_command` → `resolve_reply` pipeline as any typed/dictated instruction when tapped,
    /// reusing the keys-vs-text resolution logic that already works there.
    pub instruction: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedPromptDescription {
    /// "yes_no" | "menu" | "freeform"
    pub kind: String,
    /// Human-readable rendering of what the pane is actually asking — also what gets spoken,
    /// so what's shown and what's said are the same text.
    pub question: String,
    /// Empty for "freeform" — nothing discrete to offer as a quick-reply button.
    pub options: Vec<BlockedOption>,
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

    let context = format!(
        "Operator instruction (context only — do not repeat this back): {}\n\nAgent output:\n{}",
        dispatch.sent_content, dispatch.raw_output
    );
    let system = classification_system_prompt(settings.tts_max_words, false);
    let classification =
        classify_and_summarize(&openrouter, &settings.summarize_model, &system, &context).await?;

    let audio_base64 = if should_speak(&settings, &classification.category) {
        synthesize(&openrouter, &settings, &classification.summary).await?
    } else {
        String::new()
    };

    Ok(SendCommandResult {
        summary: classification.summary,
        category: classification.category,
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

    let gathered = supervisor_tools::gather_context(
        &collie,
        &openrouter,
        &settings.reply_model,
        &settings.summarize_model,
        &snapshot,
        &listing,
        &text,
    )
    .await?;

    let decision = supervisor_decide(
        &openrouter,
        &settings.reply_model,
        &listing,
        &gathered,
        &text,
    )
    .await?;

    if decision.action == "answer" {
        // A direct answer to a direct question (e.g. "what's blocked") — not a dispatch
        // outcome, so it isn't one of the three categories and isn't gated by their toggles.
        let answer = decision
            .answer
            .unwrap_or_else(|| "no status available".into());
        let audio_base64 = synthesize(&openrouter, &settings, &answer).await?;
        return Ok(SupervisorResult {
            summary: answer,
            category: String::new(),
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

    let context = dispatches_context(&dispatches);
    let system = classification_system_prompt(settings.tts_max_words, true);
    let classification =
        classify_and_summarize(&openrouter, &settings.summarize_model, &system, &context).await?;

    let audio_base64 = if should_speak(&settings, &classification.category) {
        synthesize(&openrouter, &settings, &classification.summary).await?
    } else {
        String::new()
    };

    Ok(SupervisorResult {
        summary: classification.summary,
        category: classification.category,
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

/// Classifies what a blocked pane is actually asking — a yes/no question, a numbered/lettered
/// menu, or an open-ended question with nothing discrete to offer — so the blocked-attention
/// overlay can render the right quick-reply buttons instead of always assuming yes/no.
#[tauri::command]
pub async fn describe_blocked_prompt(
    app: AppHandle,
    pane_id: String,
) -> Result<BlockedPromptDescription, String> {
    let settings = settings::load(&app)?;
    if settings.openrouter_api_key.trim().is_empty() {
        return Err("set an OpenRouter API key in Settings first".into());
    }
    let collie = CollieClient::new(settings.collie_base_url.clone());
    let openrouter = OpenRouterClient::new(settings.openrouter_api_key.clone());

    let tail = collie
        .read_pane(&pane_id, Some(60))
        .await
        .map_err(|e| e.to_string())?
        .text;

    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": ["yes_no", "menu", "freeform"] },
            "question": { "type": "string" },
            "options": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "label": { "type": "string" },
                        "instruction": { "type": "string" }
                    },
                    "required": ["label", "instruction"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["kind", "question", "options"],
        "additionalProperties": false
    });
    let system = "A terminal pane running an AI coding agent is waiting on the operator. Look \
        at its recent output and classify what it's asking. \"yes_no\": a plain yes/no \
        confirmation — `options` must be exactly [{label:\"Yes\",instruction:\"yes\"}, \
        {label:\"No\",instruction:\"no\"}]. \"menu\": a numbered/lettered list of discrete \
        choices — one option per choice, `instruction` phrased as free text naming that choice \
        (e.g. \"choose src/users/users.service.ts\"), NOT raw keys — something already sent \
        through voice/typed resolution. \"freeform\": an open-ended question with no discrete \
        choices to offer — leave `options` empty. `question` is a short, clear, human-readable \
        rendering of what's actually being asked, suitable to both show as text and read aloud \
        verbatim — not a restatement of raw terminal output. CRITICAL: `question` must name the \
        actual specific thing being decided, not the tool's generic chrome text. If the pane \
        shows something like \"This command requires approval\" or \"Do you want to proceed?\" \
        around an actual command/action (e.g. a shell command, a file edit, a URL), the \
        operator cannot act on the generic sentence alone — `question` must include that \
        specific command/action verbatim, e.g. \"Approve running: sleep 5 && gh run list \
        --branch main --limit 3?\" or \"Approve editing src/main.rs?\", never just \"approve \
        this command?\" with no indication of what the command actually is.";
    let value = openrouter
        .chat_json(
            &settings.reply_model,
            system,
            &tail,
            "collie_blocked_prompt",
            schema,
        )
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| format!("blocked-prompt classification: {e}"))
}

/// If no OpenRouter key is cached yet, fetches the OpenRouter *management* key from kv_manager
/// and uses it to mint a fresh, spend-capped OpenRouter key for this install, caching the
/// result in Settings so every call after this one is the free/instant already-cached path.
#[tauri::command]
pub async fn ensure_openrouter_key(app: AppHandle) -> Result<Settings, String> {
    let mut settings = settings::load(&app)?;
    if !settings.openrouter_api_key.trim().is_empty() {
        return Ok(settings);
    }
    if settings.kv_manager_base_url.trim().is_empty()
        || settings.kv_manager_api_key.trim().is_empty()
    {
        return Err(
            "no OpenRouter key cached and kv_manager isn't configured — set one in Settings".into(),
        );
    }

    let kv = KvManagerClient::new(
        settings.kv_manager_base_url.clone(),
        settings.kv_manager_api_key.clone(),
    );
    let management_key = kv
        .get_entry(&settings.kv_manager_entry_key)
        .await
        .map_err(|e| e.to_string())?;

    let http = reqwest::Client::new();
    let new_key = openrouter::provision_scoped_key(&http, &management_key, "collie-voice-commands")
        .await
        .map_err(|e| e.to_string())?;

    settings.openrouter_api_key = new_key;
    settings::save(&app, &settings)?;
    Ok(settings)
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

/// Gates whether a classified outcome actually gets synthesized to audio — every turn is still
/// classified and summarized regardless (the transcript always shows the text).
fn should_speak(settings: &Settings, category: &str) -> bool {
    match category {
        "issue" => settings.speak_issue_reports,
        "success" => settings.speak_success_reports,
        "decision_needed" => settings.speak_decision_needed,
        // Unrecognized category (a model that ignores the enum) — err toward speaking it
        // rather than silently dropping something the operator might need to hear.
        _ => true,
    }
}

fn classification_system_prompt(max_words: u32, multi_pane: bool) -> String {
    let scope_note = if multi_pane {
        " This may cover more than one pane — name each pane you mention in the summary."
    } else {
        ""
    };
    format!(
        "You produce a SPOKEN status update for an operator who just told a coding agent to do \
         something and stepped away — this is read aloud by text-to-speech, not displayed as \
         text to read. Never restate the operator's own instruction back to them; they already \
         know what they asked for — reading it back to them is pointless. Classify the outcome \
         as exactly one of: \"success\" (the task completed, nothing needed from the operator), \
         \"issue\" (something failed or went wrong), or \"decision_needed\" (the agent is \
         blocked, asking a question, or needs the operator to choose or confirm something). \
         Write `summary` as the spoken line itself — strong summarization, ONLY the absolute \
         necessities: what was accomplished or what went wrong, and if a decision is needed, \
         exactly what the operator must decide. CRITICAL: be concrete, never vague — name the \
         actual file/command/error/value involved. \"the task completed\" or \"something needs \
         your attention\" or \"a command needs approval\" are USELESS to someone who can't see \
         the screen; \"created src/auth.rs and all tests pass\" or \"npm install failed: \
         EACCES on /usr/local/lib\" or \"approve running rm -rf build/?\" are what's actually \
         needed. No filler, no pleasantries, no restating the request.{scope_note} Hard limit: \
         {max_words} words or fewer."
    )
}

async fn classify_and_summarize(
    openrouter: &OpenRouterClient,
    model: &str,
    system: &str,
    context: &str,
) -> Result<Classification, String> {
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "category": { "type": "string", "enum": ["success", "issue", "decision_needed"] },
            "summary": { "type": "string" }
        },
        "required": ["category", "summary"],
        "additionalProperties": false
    });
    let value = openrouter
        .chat_json(model, system, context, "collie_classification", schema)
        .await
        .map_err(|e| e.to_string())?;
    serde_json::from_value(value).map_err(|e| format!("classification response: {e}"))
}

fn dispatches_context(dispatches: &[PaneDispatchResult]) -> String {
    let mut combined = String::new();
    for d in dispatches {
        let name = d.pane_name.as_deref().unwrap_or(&d.pane_id);
        combined.push_str(&format!(
            "### {name}\ninstruction (context only — do not repeat this back): {}\noutput:\n{}\n\n",
            d.sent_content, d.raw_output
        ));
    }
    combined
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
    find_pane_by_id(snapshot, pane_id)
        .map(pane_display_name)
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
    gathered_context: &str,
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
        status, cwd), context already gathered by an earlier lookup step (search/summarize/\
        resolve-by-context tool calls — may be empty if no lookup was needed), and an operator \
        instruction. If the gathered context already answers the operator's question, respond \
        with action \"answer\" using it — don't dispatch to a pane to re-fetch something \
        already found. If the instruction can otherwise be answered directly from the fleet \
        status shown (e.g. \"what's blocked\", \"how many are working\"), also use \"answer\". \
        Only use \"answer\" when you're not contacting any pane. If the instruction should be \
        carried out on one or more specific panes (referred to by name, label, workspace, or \
        clearly implied — use the gathered context to resolve fuzzy references to a real pane \
        id), respond with action \"dispatch\" and list each target pane's id plus an \
        instruction phrased for that pane, leaving `answer` null. Only target panes that \
        actually exist in the listing.";
    let user = format!(
        "Fleet:\n{fleet_listing}\n\nGathered context (from tool lookups, if any):\n{gathered_context}\n\nOperator said: {user_text}"
    );
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
