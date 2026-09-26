use crate::error::{AppError, AppResult};
use crate::vault;
use crate::wallet_store;
use serde::Serialize;

#[derive(Serialize)]
pub struct ApiKeyRow {
    pub id: i64,
    pub provider: String,
    pub masked: String,
    pub base_url: Option<String>,
}

fn mask_key(k: &str) -> String {
    let chars: Vec<char> = k.chars().collect();
    if chars.len() <= 8 {
        return "••••".into();
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

#[tauri::command]
pub fn api_key_set(provider: String, key: String, base_url: Option<String>) -> AppResult<()> {
    if provider.trim().is_empty() {
        return Err(AppError::Invalid("provider required".into()));
    }
    if key.trim().is_empty() {
        return Err(AppError::Invalid("key required".into()));
    }
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let (nonce, ct) = vault::encrypt(key.trim().as_bytes())?;
    crate::db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO api_keys(provider, key_enc, nonce, base_url) VALUES (?1,?2,?3,?4)
             ON CONFLICT(provider) DO UPDATE SET key_enc=excluded.key_enc, nonce=excluded.nonce, base_url=excluded.base_url",
            rusqlite::params![provider.trim(), ct, nonce, base_url],
        )?;
        Ok(())
    })?;
    wallet_store::log_activity(
        "api_key.set",
        &format!("Saved API key for provider `{}`", provider.trim()),
        None,
        true,
    );
    Ok(())
}

#[tauri::command]
pub fn api_key_list() -> AppResult<Vec<ApiKeyRow>> {
    if !vault::is_unlocked()? {
        // still show providers without decrypting? show empty for safety
        return Ok(vec![]);
    }
    crate::db::with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, provider, key_enc, nonce, base_url FROM api_keys ORDER BY provider")?;
        let mut out = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let provider: String = row.get(1)?;
            let ct: Vec<u8> = row.get(2)?;
            let nonce: Vec<u8> = row.get(3)?;
            let base_url: Option<String> = row.get(4)?;
            let plain = vault::decrypt(&nonce, &ct).unwrap_or_default();
            let key = String::from_utf8_lossy(&plain).to_string();
            out.push(ApiKeyRow {
                id,
                provider,
                masked: mask_key(&key),
                base_url,
            });
        }
        Ok(out)
    })
}

#[tauri::command]
pub fn api_key_delete(id: i64) -> AppResult<()> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute("DELETE FROM api_keys WHERE id = ?1", [id])?)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("api key {id}")));
    }
    wallet_store::log_activity("api_key.delete", &format!("Deleted API key #{id}"), None, true);
    Ok(())
}
