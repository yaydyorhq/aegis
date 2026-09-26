mod commands;
mod db;
mod error;
mod vault;
mod wallet;
mod wallet_store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let db_path = dir.join("aevora.db");
            db::init(&db_path)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::vault::vault_status,
            commands::vault::vault_unlock,
            commands::vault::vault_lock,
            commands::vault::wallet_list,
            commands::vault::wallet_generate,
            commands::vault::wallet_import,
            commands::vault::wallet_delete,
            commands::vault::wallet_export,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
