mod collie;
mod commands;
mod kvmanager;
mod openrouter;
mod settings;
mod supervisor_tools;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
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
            commands::ensure_openrouter_key,
            commands::describe_blocked_prompt,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
