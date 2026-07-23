//! Gives the supervisor real tools (search / summarize / resolve-by-context) to ground its
//! dispatch/answer decision in what's actually in a pane, instead of guessing — the bug this
//! fixes: asked to relay "what research did the Offenleg agent come back with," the supervisor
//! previously just wrote "find research results" as a blind instruction to the pane rather than
//! actually reading its history.
//!
//! Two-phase design: this module's `gather_context` runs a real multi-turn tool-calling loop
//! (OpenRouter's `tools`, not structured output — the two don't reliably combine in one call)
//! and produces a plain-text summary of what it found. `commands::supervisor_decide` then makes
//! its existing schema-constrained dispatch/answer decision fed that summary as extra context —
//! this module never touches that decision itself.

use serde_json::Value;

use crate::collie::{pane_display_name, CollieClient, SnapshotResponse};
use crate::openrouter::OpenRouterClient;

const MAX_TOOL_ITERATIONS: u32 = 4;
const TOOL_READ_LINES: u32 = 2000; // much larger than the 200-line tail used for dispatch/resolve

const GATHER_SYSTEM_PROMPT: &str = "You are gathering information before a supervisor decides \
    what to do about a fleet of terminal panes, each possibly running an AI coding agent. You \
    have tools to search a pane's history, get a focused summary of a pane's history, or \
    resolve a fuzzy/colloquial description to an actual pane id. Use them as needed to ground \
    your understanding in what's actually there — don't guess, and don't fabricate findings. If \
    the operator's request doesn't require looking anything up, say so plainly. When you're \
    done (or no tool is relevant), respond with a short plain-text account of what you found — \
    this becomes context for the actual decision, not the final answer shown to the operator.";

/// Runs the tool-calling loop and returns a plain-text account of what it found, to be fed into
/// the existing supervisor decision call as extra context.
pub async fn gather_context(
    collie: &CollieClient,
    openrouter: &OpenRouterClient,
    model: &str,
    summarize_model: &str,
    snapshot: &SnapshotResponse,
    fleet_listing: &str,
    user_text: &str,
) -> Result<String, String> {
    let mut messages = vec![
        serde_json::json!({ "role": "system", "content": GATHER_SYSTEM_PROMPT }),
        serde_json::json!({
            "role": "user",
            "content": format!("Fleet:\n{fleet_listing}\n\nOperator said: {user_text}")
        }),
    ];

    for _ in 0..MAX_TOOL_ITERATIONS {
        let body = serde_json::json!({
            "model": model,
            "messages": messages,
            "tools": tool_definitions(),
        });
        let response = openrouter.chat_raw(body).await.map_err(|e| e.to_string())?;
        let message = response["choices"][0]["message"].clone();
        let tool_calls = message["tool_calls"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if tool_calls.is_empty() {
            return Ok(message["content"].as_str().unwrap_or_default().to_string());
        }

        messages.push(message);
        for call in &tool_calls {
            let id = call["id"].as_str().unwrap_or_default().to_string();
            let name = call["function"]["name"].as_str().unwrap_or_default();
            let args: Value = call["function"]["arguments"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let content = execute_tool(collie, openrouter, summarize_model, snapshot, name, &args)
                .await
                .unwrap_or_else(|e| format!("tool error: {e}"));
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": id,
                "content": content,
            }));
        }
    }
    Ok("(no conclusive findings — gave up after the tool-call limit)".to_string())
}

fn tool_definitions() -> Value {
    serde_json::json!([
        {
            "type": "function",
            "function": {
                "name": "search_agent_conversation",
                "description": "Search a specific pane's terminal history for a keyword or phrase. Use this to find something mentioned earlier instead of guessing or re-asking the agent.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paneId": { "type": "string", "description": "Pane id, from the fleet listing." },
                        "query": { "type": "string", "description": "Keyword or phrase to search for." }
                    },
                    "required": ["paneId", "query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "summarize_agent_conversation",
                "description": "Fetch a large chunk of a pane's terminal history and get a focused summary of it. Use this to understand what an agent has been doing or concluded, on a specific topic.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "paneId": { "type": "string" },
                        "focus": { "type": "string", "description": "What to focus the summary on, e.g. 'research findings' or 'test results'." }
                    },
                    "required": ["paneId", "focus"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "find_agent_by_context",
                "description": "Resolve a fuzzy/colloquial description of a pane (e.g. 'the Offenleg agent') to its actual pane id, when it's not obvious from the fleet listing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "description": { "type": "string" }
                    },
                    "required": ["description"]
                }
            }
        }
    ])
}

async fn execute_tool(
    collie: &CollieClient,
    openrouter: &OpenRouterClient,
    summarize_model: &str,
    snapshot: &SnapshotResponse,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    match name {
        "search_agent_conversation" => {
            let pane_id = args["paneId"].as_str().ok_or("missing paneId")?;
            let query = args["query"].as_str().ok_or("missing query")?;
            search_agent_conversation(collie, pane_id, query).await
        }
        "summarize_agent_conversation" => {
            let pane_id = args["paneId"].as_str().ok_or("missing paneId")?;
            let focus = args["focus"].as_str().ok_or("missing focus")?;
            summarize_agent_conversation(collie, openrouter, summarize_model, pane_id, focus).await
        }
        "find_agent_by_context" => {
            let description = args["description"].as_str().ok_or("missing description")?;
            Ok(find_agent_by_context(snapshot, description))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

async fn search_agent_conversation(
    collie: &CollieClient,
    pane_id: &str,
    query: &str,
) -> Result<String, String> {
    let text = collie
        .read_pane(pane_id, Some(TOOL_READ_LINES))
        .await
        .map_err(|e| e.to_string())?
        .text;
    let needle = query.to_lowercase();
    let lines: Vec<&str> = text.lines().collect();
    let mut matches = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if line.to_lowercase().contains(&needle) {
            let start = i.saturating_sub(2);
            let end = (i + 3).min(lines.len());
            matches.push(lines[start..end].join("\n"));
            if matches.len() >= 5 {
                break;
            }
        }
    }
    if matches.is_empty() {
        return Ok(format!("no matches for \"{query}\" in pane {pane_id}"));
    }
    Ok(matches.join("\n---\n"))
}

async fn summarize_agent_conversation(
    collie: &CollieClient,
    openrouter: &OpenRouterClient,
    summarize_model: &str,
    pane_id: &str,
    focus: &str,
) -> Result<String, String> {
    let text = collie
        .read_pane(pane_id, Some(TOOL_READ_LINES))
        .await
        .map_err(|e| e.to_string())?
        .text;
    let schema = serde_json::json!({
        "type": "object",
        "properties": { "summary": { "type": "string" } },
        "required": ["summary"],
        "additionalProperties": false
    });
    let system = format!(
        "Summarize this terminal pane's output, focused specifically on: {focus}. Be concise but \
         include the concrete findings/results, not just that work happened."
    );
    let value = openrouter
        .chat_json(summarize_model, &system, &text, "tool_summary", schema)
        .await
        .map_err(|e| e.to_string())?;
    value["summary"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "no summary in tool response".to_string())
}

/// Pure Rust, no LLM call — scores every pane by keyword overlap between `description` and its
/// agent/label/workspace/cwd. Fast and free; an explicit tool call is more reliable than
/// expecting the model to parse the whole fleet listing text blob perfectly on every turn.
fn find_agent_by_context(snapshot: &SnapshotResponse, description: &str) -> String {
    let terms: Vec<String> = description
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();

    let mut scored: Vec<(u32, String, String)> = snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .filter_map(|p| {
            let haystack = format!(
                "{} {} {} {} {}",
                p.agent,
                p.pane_label.as_deref().unwrap_or(""),
                p.session_name.as_deref().unwrap_or(""),
                p.workspace_label,
                p.cwd
            )
            .to_lowercase();
            let score = terms
                .iter()
                .filter(|t| haystack.contains(t.as_str()))
                .count() as u32;
            (score > 0).then(|| (score, p.pane_id.clone(), pane_display_name(p)))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(5);

    if scored.is_empty() {
        return format!("no panes matched \"{description}\"");
    }
    scored
        .into_iter()
        .map(|(score, id, name)| format!("paneId={id} name={name} matchScore={score}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_snapshot() -> SnapshotResponse {
        let json = serde_json::json!({
            "bridge": "connected",
            "agents": [
                {
                    "paneId": "pane-1",
                    "workspaceId": "ws-1",
                    "workspaceLabel": "offenlegung",
                    "workspaceNumber": 1,
                    "tabId": "tab-1",
                    "agent": "claude",
                    "status": "done",
                    "cwd": "/home/rafa/dev/Offenleg-Stufe-3",
                    "focused": false,
                    "sessionName": "research"
                },
                {
                    "paneId": "pane-2",
                    "workspaceId": "ws-2",
                    "workspaceLabel": "billing",
                    "workspaceNumber": 2,
                    "tabId": "tab-2",
                    "agent": "claude",
                    "status": "idle",
                    "cwd": "/home/rafa/dev/kv_manager",
                    "focused": false
                }
            ],
            "shellPanes": [],
            "workspaces": [],
            "tabs": [],
            "sessions": [],
            "ts": 1234
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn finds_pane_by_fuzzy_workspace_or_cwd_match() {
        let snapshot = fixture_snapshot();
        let result = find_agent_by_context(&snapshot, "the Offenleg agent");
        assert!(result.contains("pane-1"), "expected pane-1 in: {result}");
        assert!(
            !result.contains("pane-2"),
            "did not expect pane-2 in: {result}"
        );
    }

    #[test]
    fn no_match_says_so() {
        let snapshot = fixture_snapshot();
        let result = find_agent_by_context(&snapshot, "nonexistent thing entirely");
        assert!(result.starts_with("no panes matched"));
    }

    #[test]
    fn ranks_stronger_match_first() {
        let snapshot = fixture_snapshot();
        let result = find_agent_by_context(&snapshot, "billing kv_manager");
        let first_line = result.lines().next().unwrap();
        assert!(
            first_line.contains("pane-2"),
            "expected pane-2 first in: {result}"
        );
    }
}
