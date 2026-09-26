use crate::error::AppResult;

#[tauri::command]
pub fn meta_get(key: String) -> AppResult<Option<String>> {
    crate::db::meta_get(&key)
}

#[tauri::command]
pub fn meta_set(key: String, value: String) -> AppResult<()> {
    // Only allow a small allowlist of keys from the frontend.
    match key.as_str() {
        "profile_name" | "sidebar_collapsed" | "theme" => crate::db::meta_set(&key, &value),
        other => Err(crate::error::AppError::Invalid(format!(
            "unknown meta key: {other}"
        ))),
    }
}
