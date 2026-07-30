use tauri::AppHandle;

use crate::collie::{
    BlockedPromptDescription, CollieClient, PaneDispatchResult, PaneReadResponse,
    SendCommandResult, SnapshotResponse, SupervisorResult, Todo, TodoStatus,
};
use crate::settings::{self, Settings};

#[tauri::command]
pub fn get_settings(app: AppHandle) -> Result<Settings, String> {
    settings::load(&app)
}

#[tauri::command]
pub fn save_settings(app: AppHandle, new_settings: Settings) -> Result<(), String> {
    settings::save(&app, &new_settings)
}

/// Thin proxy to collie-server's `POST /api/command` — all reply resolution, dispatch,
/// classification, summarization and TTS now happen server-side.
#[tauri::command]
pub async fn send_command(
    app: AppHandle,
    text: String,
    pane_id: Option<String>,
) -> Result<SendCommandResult, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .send_command(&text, pane_id.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Thin proxy to collie-server's `POST /api/supervisor/command` — fleet-wide routing decision,
/// dispatch, classification, summarization and TTS now all happen server-side.
#[tauri::command]
pub async fn send_supervisor_command(
    app: AppHandle,
    text: String,
) -> Result<SupervisorResult, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .send_supervisor_command(&text)
        .await
        .map_err(|e| e.to_string())
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

/// Thin proxy to collie-server's `POST /api/pane/:id/blocked` — classifies what a blocked pane
/// is actually asking (yes/no, menu, or freeform) so the blocked-attention overlay can render
/// the right quick-reply buttons.
#[tauri::command]
pub async fn describe_blocked_prompt(
    app: AppHandle,
    pane_id: String,
) -> Result<BlockedPromptDescription, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .describe_blocked_prompt(&pane_id)
        .await
        .map_err(|e| e.to_string())
}

/// Thin proxy to collie-server's `POST /api/speak` — synthesizes speech for arbitrary text
/// (e.g. the "pane needs you" attention alert) via collie-server's own OpenRouter TTS config.
#[tauri::command]
pub async fn speak(app: AppHandle, text: String) -> Result<String, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie.speak(&text).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_todos(app: AppHandle) -> Result<Vec<Todo>, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie.list_todos().await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn create_todo(
    app: AppHandle,
    title: String,
    description: Option<String>,
    parent_id: Option<String>,
) -> Result<Todo, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .create_todo(&title, description.as_deref(), parent_id.as_deref())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn patch_todo(
    app: AppHandle,
    id: String,
    title: Option<String>,
    description: Option<Option<String>>,
    status: Option<TodoStatus>,
) -> Result<Todo, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .patch_todo(
            &id,
            title.as_deref(),
            description.as_ref().map(|d| d.as_deref()),
            status,
        )
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn delete_todo(app: AppHandle, id: String) -> Result<(), String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie.delete_todo(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn split_todo(app: AppHandle, id: String) -> Result<Vec<Todo>, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie.split_todo(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn dispatch_todo(
    app: AppHandle,
    id: String,
    pane_id: String,
) -> Result<PaneDispatchResult, String> {
    let settings = settings::load(&app)?;
    let collie = CollieClient::new(settings.collie_base_url);
    collie
        .dispatch_todo(&id, &pane_id)
        .await
        .map_err(|e| e.to_string())
}
