// Background monitor: the single always-on poller for "does anything need me right now."
// Runs once, in Rust, every 6s — the same cadence app.js used to poll on its own — started once
// in `lib.rs`'s `setup()`. The background-service plugin's only job is keeping this loop alive
// when Android backgrounds the app; it is not a second, independent polling mechanism. `app.js`
// no longer runs its own `setInterval` poll for blocked-pane detection — it listens for the
// `collie://snapshot` event this loop emits each cycle, keeping `get_snapshot` as an on-demand
// command only (first paint, manual refresh). This makes `MonitorState` the single source of
// truth for "what's blocked," so JS's old `prevStatus`/`dismissedBlocked` bookkeeping doesn't
// need to be kept in sync with a second copy.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use tauri::{Emitter, Manager};
use tauri_plugin_background_service::{BackgroundService, ServiceContext, ServiceError};

use crate::collie::CollieClient;
use crate::settings;

const POLL_INTERVAL: Duration = Duration::from_secs(6);

struct PaneAlertState {
    notified_at: Instant,
    escalated: bool,
    question: String,
}

/// Tracks which panes are currently blocked and how far along their notify→escalate timeline
/// each one is. Managed via `app.manage(...)`; `poll_once` is the only writer.
#[derive(Default)]
pub struct MonitorState {
    panes: Mutex<HashMap<String, PaneAlertState>>,
    /// The pane id from the most recent notification fired, consumed once by `app.js`'s
    /// `init()` via `take_pending_navigation()` to jump straight to that pane's chat view. This
    /// is an approximation of "notification tap → deep link": the background-service plugin's
    /// `Notifier` only exposes a plain title/body `show()` (confirmed against its API
    /// reference), with no click-intent/extras hook to detect a real tap vs. a normal cold
    /// launch. Good enough for "the app was just opened after being alerted," not a precise tap
    /// signal.
    pending_navigation: Mutex<Option<String>>,
}

/// What the foreground app is currently showing, set via `set_foreground_context` from
/// `app.js`'s `visibilitychange` handler and pane switches. Lets `poll_once` skip firing an OS
/// notification for the one pane the user is already looking at in an open, visible app — the
/// in-webview blocked overlay already covers that case. Every other blocked pane, and
/// everything while the app isn't visible, still notifies normally.
#[derive(Default)]
pub struct ForegroundContext {
    inner: Mutex<ForegroundContextInner>,
}

#[derive(Default)]
struct ForegroundContextInner {
    visible: bool,
    viewing_pane_id: Option<String>,
}

impl ForegroundContext {
    fn suppresses(&self, pane_id: &str) -> bool {
        let ctx = self.inner.lock().unwrap();
        ctx.visible && ctx.viewing_pane_id.as_deref() == Some(pane_id)
    }
}

#[tauri::command]
pub fn set_foreground_context(
    foreground: tauri::State<ForegroundContext>,
    visible: bool,
    viewing_pane_id: Option<String>,
) {
    let mut ctx = foreground.inner.lock().unwrap();
    ctx.visible = visible;
    ctx.viewing_pane_id = viewing_pane_id;
}

#[tauri::command]
pub fn take_pending_navigation(monitor: tauri::State<MonitorState>) -> Option<String> {
    monitor.pending_navigation.lock().unwrap().take()
}

pub struct Monitor;

impl Monitor {
    pub fn new() -> Self {
        Self
    }
}

// Concrete `tauri::Wry` rather than generic `R: Runtime` — this app only ever runs on the
// default Wry runtime (`tauri::Builder::default()`), and pinning it here lets `poll_once` take
// a plain `&AppHandle` (matching `settings::load`'s existing signature, used elsewhere in this
// app) instead of threading a runtime type parameter through every function that touches
// settings or the app handle.
#[async_trait]
impl BackgroundService<tauri::Wry> for Monitor {
    async fn init(&mut self, _ctx: &ServiceContext<tauri::Wry>) -> Result<(), ServiceError> {
        Ok(())
    }

    async fn run(&mut self, ctx: &ServiceContext<tauri::Wry>) -> Result<(), ServiceError> {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            tokio::select! {
                _ = ctx.shutdown.cancelled() => break,
                _ = interval.tick() => {
                    if let Err(e) = poll_once(&ctx.app, &ctx.notifier).await {
                        log::warn!("monitor poll_once failed: {e:#}");
                    }
                }
            }
        }
        Ok(())
    }
}

/// One poll cycle: fetch the snapshot, notify newly-blocked panes (unless the user's already
/// looking at that exact pane in a visible app), escalate to TTS for panes that have been
/// blocked past the configured timeout with no response, and clear tracked state for panes no
/// longer blocked. Plugin-agnostic — takes a `Notifier` by reference so whatever keeps the
/// process alive (the background-service plugin today, or a hand-rolled Kotlin `Service`
/// fallback later) can call this the same way without the monitoring logic itself changing.
pub async fn poll_once(
    app: &tauri::AppHandle,
    notifier: &tauri_plugin_background_service::Notifier<tauri::Wry>,
) -> anyhow::Result<()> {
    let settings = settings::load(app).map_err(|e| anyhow::anyhow!(e))?;
    let timeout = Duration::from_secs(settings.notification_response_timeout_secs as u64);
    let collie = CollieClient::new(settings.collie_base_url);

    let snapshot = collie.snapshot().await?;
    let _ = app.emit("collie://snapshot", &snapshot);

    let monitor = app.state::<MonitorState>();
    let foreground = app.state::<ForegroundContext>();

    let blocked_ids: std::collections::HashSet<String> = snapshot
        .agents
        .iter()
        .chain(snapshot.shell_panes.iter())
        .filter(|p| p.status == crate::collie::AgentStatus::Blocked)
        .map(|p| p.pane_id.clone())
        .collect();

    for pane in snapshot.agents.iter().chain(snapshot.shell_panes.iter()) {
        if pane.status != crate::collie::AgentStatus::Blocked {
            continue;
        }
        let pane_id = pane.pane_id.clone();
        let already_tracked = monitor.panes.lock().unwrap().contains_key(&pane_id);

        if !already_tracked {
            let question = match collie.describe_blocked_prompt(&pane_id).await {
                Ok(desc) => desc.question,
                Err(e) => {
                    log::warn!("describe_blocked_prompt failed for {pane_id}: {e:#}");
                    continue;
                }
            };
            monitor.panes.lock().unwrap().insert(
                pane_id.clone(),
                PaneAlertState {
                    notified_at: Instant::now(),
                    escalated: false,
                    question: question.clone(),
                },
            );
            if !foreground.suppresses(&pane_id) {
                let name = crate::collie::pane_display_name(pane);
                notifier.show(&format!("{name} needs you"), &question);
                *monitor.pending_navigation.lock().unwrap() = Some(pane_id.clone());
            }
            continue;
        }

        // Escalate panes that have been blocked past the timeout without a response — this
        // check runs regardless of foreground suppression, since a user who left the pane
        // blocked and walked away with the app still open in the background should still get
        // the audible escalation.
        let should_escalate = {
            let mut panes = monitor.panes.lock().unwrap();
            let state = panes.get_mut(&pane_id).unwrap();
            !state.escalated && state.notified_at.elapsed() >= timeout
        };
        if should_escalate {
            let question = monitor
                .panes
                .lock()
                .unwrap()
                .get(&pane_id)
                .unwrap()
                .question
                .clone();
            match collie.speak(&question).await {
                Ok(audio_url) => {
                    // Mirror UI state if the app happens to be open; JS's existing playAudio()
                    // pipeline (app.js `playAudio`/`primeAudioPlayback`) is reused unchanged for
                    // actual playback rather than rebuilt in Rust via `rodio` — this is the
                    // simpler of the two paths the plan called out, chosen as the starting
                    // implementation. Whether it actually produces audible speaker output with
                    // the screen off and the app backgrounded is the open question step 2 of the
                    // build order calls for testing empirically on-device; if that test shows
                    // the WebView doesn't play audio while backgrounded, this is the point to
                    // swap in native `rodio` playback instead.
                    let _ = app.emit(
                        "collie://tts-playing",
                        serde_json::json!({ "paneId": pane_id, "audioUrl": audio_url, "text": question }),
                    );
                    if let Some(state) = monitor.panes.lock().unwrap().get_mut(&pane_id) {
                        state.escalated = true;
                    }
                }
                Err(e) => log::warn!("speak failed for {pane_id}: {e:#}"),
            }
        }
    }

    monitor
        .panes
        .lock()
        .unwrap()
        .retain(|pane_id, _| blocked_ids.contains(pane_id));

    Ok(())
}
