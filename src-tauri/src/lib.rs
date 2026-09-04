mod collie;
mod commands;
mod monitor;
mod settings;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() -> anyhow::Result<()> {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_background_service::init_with_service(|| {
            monitor::Monitor::new()
        }))
        .manage(monitor::MonitorState::default())
        .manage(monitor::ForegroundContext::default())
        .setup(|app| {
            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::send_command,
            commands::send_supervisor_command,
            commands::get_snapshot,
            commands::read_pane,
            commands::speak,
            commands::describe_blocked_prompt,
            commands::list_todos,
            commands::create_todo,
            commands::patch_todo,
            commands::delete_todo,
            commands::split_todo,
            commands::dispatch_todo,
            monitor::set_foreground_context,
            monitor::take_pending_navigation,
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}
