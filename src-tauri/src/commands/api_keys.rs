use crate::error::{AppError, AppResult};
use crate::vault;
use crate::wallet_store;
use serde::Serialize;
use zeroize::Zeroizing;

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
    // M02: decrypt into a Zeroizing buffer so the plaintext bytes are wiped
    // on drop instead of lingering in an owned String on the heap.  The mask
    // borrows from the Zeroizing buffer (Cow<str> → &str, no copy for valid
    // UTF-8) and only head[4] + tail[4] chars survive in the returned String.
    //
    // M03: a decrypt failure marks the row `<unreadable>` instead of
    // `unwrap_or_default()` → "" → which renders identically to a short key.
    // Corrupt providers are collected here and logged AFTER the connection
    // guard drops — log_activity re-acquires the same global DB mutex and
    // would deadlock inside this closure.
    let (rows, corrupt) = crate::db::with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, provider, key_enc, nonce, base_url FROM api_keys ORDER BY provider")?;
        let mut out = Vec::new();
        let mut corrupt: Vec<String> = Vec::new();
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let provider: String = row.get(1)?;
            let ct: Vec<u8> = row.get(2)?;
            let nonce: Vec<u8> = row.get(3)?;
            let base_url: Option<String> = row.get(4)?;
            let masked = match vault::decrypt(&nonce, &ct) {
                Ok(bytes) => mask_key(&String::from_utf8_lossy(&Zeroizing::new(bytes))),
                Err(_) => {
                    corrupt.push(provider.clone());
                    "<unreadable>".to_string()
                }
            };
            out.push(ApiKeyRow {
                id,
                provider,
                masked,
                base_url,
            });
        }
        Ok((out, corrupt))
    })?;
    for provider in &corrupt {
        wallet_store::log_activity(
            "api_key.error",
            &format!(
                "Failed to decrypt API key for provider `{provider}` (corrupt row or wrong vault session)"
            ),
            None,
            false,
        );
    }
    Ok(rows)
}

/// Backend-internal: decrypt one stored provider key for outbound requests.
/// Never exposed via IPC — the frontend only ever sees the masked form from
/// [`api_key_list`]. Locked vault / missing row / decrypt failure → None, and
/// callers simply fall back to anonymous (keyless) requests.
pub fn lookup(provider: &str) -> Option<String> {
    if !vault::is_unlocked().unwrap_or(false) {
        return None;
    }
    let row: Option<(Vec<u8>, Vec<u8>)> = crate::db::with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT key_enc, nonce FROM api_keys WHERE provider = ?1")?;
        let mut rows = stmt.query([provider])?;
        match rows.next()? {
            Some(r) => Ok(Some((r.get(0)?, r.get(1)?))),
            None => Ok(None),
        }
    })
    .ok()
    .flatten();
    let (ct, nonce) = row?;
    let plain = vault::decrypt(&nonce, &ct).ok()?;
    String::from_utf8(plain).ok().filter(|s| !s.is_empty())
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
