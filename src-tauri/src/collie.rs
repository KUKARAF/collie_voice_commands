// These types mirror Collie's full API response shape, verified field-by-field against its
// source — not every field is consumed yet (v1 only needs `agents`/`shellPanes`/`status`), but
// they stay here as the accurate contract for features that read the rest of the snapshot later.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollieError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("collie error: {0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, CollieError>;

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Idle,
    Working,
    Blocked,
    Done,
    Unknown,
}

impl AgentStatus {
    /// The wire/display form — same as the `#[serde(rename_all = "lowercase")]` above, exposed
    /// for building LLM-facing text (the supervisor's fleet listing) without a serde round-trip.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Idle => "idle",
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BridgeStatus {
    Connected,
    Disconnected,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentView {
    pub pane_id: String,
    pub workspace_id: String,
    pub workspace_label: String,
    pub workspace_number: u32,
    pub tab_id: String,
    pub agent: String,
    pub status: AgentStatus,
    pub cwd: String,
    pub focused: bool,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub pane_label: Option<String>,
    #[serde(default)]
    pub session_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub active_tab_id: String,
    pub tab_count: u32,
    pub pane_count: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TabView {
    pub tab_id: String,
    pub workspace_id: String,
    pub number: u32,
    pub label: String,
    pub focused: bool,
    pub pane_count: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub name: String,
    pub is_primary: bool,
    pub reachable: bool,
    pub agents: u32,
    pub working: u32,
    pub blocked: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeviceAuth {
    pub enforced: bool,
    pub device: Option<String>,
    pub authorized: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateStatus {
    pub current: String,
    pub latest: Option<String>,
    pub latest_url: Option<String>,
    pub release_available: bool,
    pub bridge_stale: bool,
    pub checked_at: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationsStatus {
    pub snoozed_until: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotResponse {
    pub bridge: BridgeStatus,
    #[serde(default)]
    pub device: Option<DeviceAuth>,
    pub agents: Vec<AgentView>,
    #[serde(default)]
    pub shell_panes: Vec<AgentView>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceView>,
    #[serde(default)]
    pub tabs: Vec<TabView>,
    #[serde(default)]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub notifications: Option<NotificationsStatus>,
    #[serde(default)]
    pub update: Option<UpdateStatus>,
    pub ts: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneReadResponse {
    pub pane_id: String,
    pub text: String,
    pub truncated: bool,
    pub revision: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse {
    pub ok: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub text_delivered: Option<bool>,
}

/// Picks the pane the app should target: whichever the desktop TUI currently has
/// `focused: true`, checked in `agents` first, then `shellPanes` — matches Collie's own
/// notion of "currently active" rather than us guessing.
pub fn find_focused_pane(snapshot: &SnapshotResponse) -> Option<&AgentView> {
    snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .find(|a| a.focused)
}

/// Human-readable label for a pane — "claude · users-service" style — preferring a user-set
/// label, then Claude's own in-pane session name, falling back to just the agent name.
pub fn pane_display_name(p: &AgentView) -> String {
    let label = p
        .pane_label
        .clone()
        .or_else(|| p.session_name.clone())
        .unwrap_or_else(|| p.agent.clone());
    format!("{} · {label}", p.agent)
}

pub fn find_pane_by_id<'a>(snapshot: &'a SnapshotResponse, pane_id: &str) -> Option<&'a AgentView> {
    snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .find(|p| p.pane_id == pane_id)
}

// ---- supervisor/command wire types — mirror collie-server's src/supervisor.rs +
// src/api.rs response shapes verbatim (camelCase over the wire). collie-server now owns all
// LLM orchestration (reply resolution, fleet routing, classification, TTS); this app only
// deserializes its results. ----

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneDispatchResult {
    pub pane_id: String,
    #[serde(default)]
    pub pane_name: Option<String>,
    pub sent_mode: String,
    pub sent_content: String,
    pub pre_send_context: String,
    pub raw_output: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendCommandResult {
    pub summary: String,
    pub category: String,
    pub audio_url: String,
    pub dispatch: PaneDispatchResult,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SupervisorResult {
    pub summary: String,
    pub category: String,
    pub audio_url: String,
    pub dispatches: Vec<PaneDispatchResult>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedOption {
    pub label: String,
    pub instruction: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockedPromptDescription {
    pub kind: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<BlockedOption>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeakResponse {
    audio_url: String,
}

pub struct CollieClient {
    http: reqwest::Client,
    base_url: String,
}

impl CollieClient {
    pub fn new(base_url: String) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
        }
    }

    /// Sent as `Origin` on every write. Collie's write-path access check requires an Origin
    /// whose host matches the Host it receives, or a loopback Host — ours is never loopback
    /// over the tailnet, and with no Origin header at all a non-loopback write 403s
    /// "origin required". This is a client-side header we control, not a server reconfig.
    fn origin(&self) -> &str {
        &self.base_url
    }

    async fn ensure_success(resp: reqwest::Response) -> Result<reqwest::Response> {
        if resp.status().is_success() {
            return Ok(resp);
        }
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(CollieError::Api(format!("{status}: {body}")))
    }

    pub async fn snapshot(&self) -> Result<SnapshotResponse> {
        let url = format!("{}/api/snapshot", self.base_url);
        let resp = self.http.get(url).send().await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<SnapshotResponse>().await?)
    }

    pub async fn read_pane(&self, pane_id: &str, lines: Option<u32>) -> Result<PaneReadResponse> {
        let mut url = format!("{}/api/pane/{}", self.base_url, percent_encode(pane_id));
        if let Some(lines) = lines {
            url.push_str(&format!("?lines={lines}"));
        }
        let resp = self.http.get(url).send().await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<PaneReadResponse>().await?)
    }

    pub async fn reply(&self, pane_id: &str, text: &str) -> Result<ActionResponse> {
        let url = format!(
            "{}/api/pane/{}/reply",
            self.base_url,
            percent_encode(pane_id)
        );
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .json(&serde_json::json!({ "text": text, "submit": true }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<ActionResponse>().await?)
    }

    pub async fn keys(&self, pane_id: &str, keys: Vec<String>) -> Result<ActionResponse> {
        let url = format!(
            "{}/api/pane/{}/keys",
            self.base_url,
            percent_encode(pane_id)
        );
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .json(&serde_json::json!({ "keys": keys }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<ActionResponse>().await?)
    }

    /// Resolves + sends an instruction to one pane (or the currently focused pane, when
    /// `pane_id` is `None`) and returns the classified/summarized/spoken outcome — all of that
    /// work now happens server-side. Note: collie-server's `CommandBody` has no
    /// `rename_all = "camelCase"` attribute, so the JSON key really is `pane_id`, not `paneId`
    /// — verified against `collie-server/src/api.rs`.
    pub async fn send_command(
        &self,
        text: &str,
        pane_id: Option<&str>,
    ) -> Result<SendCommandResult> {
        let url = format!("{}/api/command", self.base_url);
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .json(&serde_json::json!({ "text": text, "pane_id": pane_id }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<SendCommandResult>().await?)
    }

    /// Fleet-wide command: collie-server decides which pane(s) to target (or answers directly
    /// from fleet status) and returns the classified/summarized/spoken outcome.
    pub async fn send_supervisor_command(&self, text: &str) -> Result<SupervisorResult> {
        let url = format!("{}/api/supervisor/command", self.base_url);
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<SupervisorResult>().await?)
    }

    /// Classifies what a blocked pane is asking (yes/no, menu, or freeform) for the
    /// blocked-attention overlay's quick-reply buttons.
    pub async fn describe_blocked_prompt(&self, pane_id: &str) -> Result<BlockedPromptDescription> {
        let url = format!(
            "{}/api/pane/{}/blocked",
            self.base_url,
            percent_encode(pane_id)
        );
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        Ok(resp.json::<BlockedPromptDescription>().await?)
    }

    /// Synthesizes speech for arbitrary text via collie-server's own OpenRouter TTS
    /// configuration — returns a relative URL (`/api/audio/<id>`) the caller can fetch/play
    /// directly rather than a base64 blob.
    pub async fn speak(&self, text: &str) -> Result<String> {
        let url = format!("{}/api/speak", self.base_url);
        let resp = self
            .http
            .post(url)
            .header("Origin", self.origin())
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await?;
        let resp = Self::ensure_success(resp).await?;
        let parsed: SpeakResponse = resp.json().await?;
        Ok(parsed.audio_url)
    }
}

/// Pane ids are placed directly in the URL path — encode defensively rather than assume
/// they're always plain alphanumerics.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snapshot_fixture() {
        let fixture = serde_json::json!({
            "bridge": "connected",
            "agents": [{
                "paneId": "pane-1",
                "workspaceId": "ws-1",
                "workspaceLabel": "main",
                "workspaceNumber": 1,
                "tabId": "tab-1",
                "agent": "claude",
                "status": "working",
                "cwd": "/home/rafa",
                "focused": true
            }],
            "shellPanes": [],
            "workspaces": [],
            "tabs": [],
            "sessions": [],
            "notifications": { "snoozedUntil": null },
            "ts": 1234
        });
        let snapshot: SnapshotResponse = serde_json::from_value(fixture).unwrap();
        assert_eq!(snapshot.bridge, BridgeStatus::Connected);
        assert_eq!(snapshot.agents.len(), 1);
        assert_eq!(snapshot.agents[0].status, AgentStatus::Working);
        assert!(find_focused_pane(&snapshot).is_some());
        assert_eq!(find_focused_pane(&snapshot).unwrap().pane_id, "pane-1");
    }

    #[test]
    fn no_focused_pane_returns_none() {
        let fixture = serde_json::json!({
            "bridge": "connected",
            "agents": [{
                "paneId": "pane-1",
                "workspaceId": "ws-1",
                "workspaceLabel": "main",
                "workspaceNumber": 1,
                "tabId": "tab-1",
                "agent": "claude",
                "status": "idle",
                "cwd": "/home/rafa",
                "focused": false
            }],
            "shellPanes": [],
            "workspaces": [],
            "tabs": [],
            "sessions": [],
            "ts": 1234
        });
        let snapshot: SnapshotResponse = serde_json::from_value(fixture).unwrap();
        assert!(find_focused_pane(&snapshot).is_none());
    }

    #[test]
    fn origin_derived_from_base_url_without_trailing_slash() {
        let client = CollieClient::new("https://thinkpad.sparidae-chinstrap.ts.net/".into());
        assert_eq!(
            client.origin(),
            "https://thinkpad.sparidae-chinstrap.ts.net"
        );
    }

    #[test]
    fn percent_encode_escapes_reserved_bytes() {
        assert_eq!(percent_encode("pane 1/x"), "pane%201%2Fx");
        assert_eq!(percent_encode("pane-1_a.b~c"), "pane-1_a.b~c");
    }
}
