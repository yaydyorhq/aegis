use crate::error::AppResult;

#[tauri::command]
pub fn meta_get(key: String) -> AppResult<Option<String>> {
    crate::db::meta_get(&key)
}

#[tauri::command]
pub fn meta_set(key: String, value: String) -> AppResult<()> {
    // Only allow a small allowlist of keys from the frontend.
    match key.as_str() {
        "profile_name"
        | "sidebar_collapsed"
        | "theme"
        | "pnl_autoscan"
        | "vault_autolock"
        | "db_retention_days" => crate::db::meta_set(&key, &value),
        other => Err(crate::error::AppError::Invalid(format!(
            "unknown meta key: {other}"
        ))),
    }
}

/// Prune append-only tables older than `retention_days` (money trails are
/// never touched — see db::prune_old_data). Manual control for the Settings
/// "Data retention" section; startup auto-prune calls db::prune_old_data
/// directly.
#[tauri::command]
pub fn db_prune(retention_days: i64) -> AppResult<crate::db::PruneReport> {
    if retention_days <= 0 {
        return Err(crate::error::AppError::Invalid(
            "retention must be at least 1 day".into(),
        ));
    }
    crate::db::prune_old_data(retention_days)
}
