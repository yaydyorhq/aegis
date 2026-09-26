use crate::db;
use crate::error::{AppError, AppResult};
use aes_gcm::aead::{Aead, KeyInit, OsRng as AeadOsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::Argon2;
use once_cell::sync::OnceCell;
use rand::RngCore;
use std::sync::Mutex;
use zeroize::Zeroizing;

pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;

struct VaultInner {
    dek: Zeroizing<[u8; 32]>,
}

static VAULT: OnceCell<Mutex<Option<VaultInner>>> = OnceCell::new();

fn vault() -> &'static Mutex<Option<VaultInner>> {
    VAULT.get_or_init(|| Mutex::new(None))
}

fn get_meta(key: &str) -> AppResult<Option<String>> {
    db::with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        if let Some(row) = rows.next()? {
            Ok(Some(row.get(0)?))
        } else {
            Ok(None)
        }
    })
}

fn set_meta(key: &str, value: &str) -> AppResult<()> {
    db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    })
}

fn derive_dek(pass: &str, salt: &[u8]) -> AppResult<Zeroizing<[u8; 32]>> {
    let salt_str = SaltString::encode_b64(salt).map_err(|e| AppError::Crypto(e.to_string()))?;
    let hash = Argon2::default()
        .hash_password(pass.as_bytes(), &salt_str)
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    let hash_bytes = hash
        .hash
        .ok_or_else(|| AppError::Crypto("missing hash".into()))?;
    let raw = hash_bytes.as_bytes();
    let mut dek = Zeroizing::new([0u8; 32]);
    let n = raw.len().min(32);
    dek[..n].copy_from_slice(&raw[..n]);
    Ok(dek)
}

pub fn is_initialized() -> AppResult<bool> {
    Ok(get_meta("vault_salt")?.is_some())
}

pub fn is_unlocked() -> AppResult<bool> {
    let guard = vault()
        .lock()
        .map_err(|_| AppError::Other("vault lock poisoned".into()))?;
    Ok(guard.is_some())
}

fn cipher_from(key: &Zeroizing<[u8; 32]>) -> Aes256Gcm {
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_slice()))
}

pub fn unlock_or_create(pass: &str) -> AppResult<()> {
    if pass.len() < 8 {
        return Err(AppError::Invalid(
            "passphrase must be at least 8 characters".into(),
        ));
    }
    let salt_b64 = get_meta("vault_salt")?;
    let salt = if let Some(s) = salt_b64 {
        hex::decode(s).map_err(|e| AppError::Crypto(e.to_string()))?
    } else {
        let mut salt = vec![0u8; SALT_LEN];
        AeadOsRng.fill_bytes(&mut salt);
        set_meta("vault_salt", &hex::encode(&salt))?;
        salt
    };

    // Verify against stored verifier if exists
    if let Some(verifier) = get_meta("vault_verifier")? {
        let dek = derive_dek(pass, &salt)?;
        let parts: Vec<&str> = verifier.split(':').collect();
        if parts.len() != 2 {
            return Err(AppError::Crypto("bad verifier".into()));
        }
        let nonce = hex::decode(parts[0]).map_err(|e| AppError::Crypto(e.to_string()))?;
        let ct = hex::decode(parts[1]).map_err(|e| AppError::Crypto(e.to_string()))?;
        let cipher = cipher_from(&dek);
        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
            .map_err(|_| AppError::BadPassphrase)?;
        if plain != b"aevora-vault-v1" {
            return Err(AppError::BadPassphrase);
        }
        let mut g = vault()
            .lock()
            .map_err(|_| AppError::Other("vault lock poisoned".into()))?;
        *g = Some(VaultInner { dek });
        return Ok(());
    }

    // First init: create verifier
    let dek = derive_dek(pass, &salt)?;
    let cipher = cipher_from(&dek);
    let mut nonce = [0u8; NONCE_LEN];
    AeadOsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), b"aevora-vault-v1".as_ref())
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    set_meta(
        "vault_verifier",
        &format!("{}:{}", hex::encode(nonce), hex::encode(ct)),
    )?;
    let mut g = vault()
        .lock()
        .map_err(|_| AppError::Other("vault lock poisoned".into()))?;
    *g = Some(VaultInner { dek });
    Ok(())
}

pub fn lock() -> AppResult<()> {
    let mut g = vault()
        .lock()
        .map_err(|_| AppError::Other("vault lock poisoned".into()))?;
    *g = None;
    Ok(())
}

fn dek() -> AppResult<Zeroizing<[u8; 32]>> {
    let g = vault()
        .lock()
        .map_err(|_| AppError::Other("vault lock poisoned".into()))?;
    match g.as_ref() {
        Some(inner) => Ok(Zeroizing::new(*inner.dek)),
        None => Err(AppError::VaultLocked),
    }
}

pub fn encrypt(pt: &[u8]) -> AppResult<(Vec<u8>, Vec<u8>)> {
    let key = dek()?;
    let cipher = cipher_from(&key);
    let mut nonce = [0u8; NONCE_LEN];
    AeadOsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), pt)
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    Ok((nonce.to_vec(), ct))
}

pub fn decrypt(nonce: &[u8], ct: &[u8]) -> AppResult<Vec<u8>> {
    let key = dek()?;
    if nonce.len() != NONCE_LEN {
        return Err(AppError::Crypto("bad nonce".into()));
    }
    let cipher = cipher_from(&key);
    cipher
        .decrypt(Nonce::from_slice(nonce), ct)
        .map_err(|_| AppError::Crypto("decrypt failed".into()))
}

pub fn confirm_passphrase(pass: &str) -> AppResult<()> {
    let salt_b64 = get_meta("vault_salt")?
        .ok_or_else(|| AppError::NotFound("vault".into()))?;
    let salt = hex::decode(salt_b64).map_err(|e| AppError::Crypto(e.to_string()))?;
    let verifier = get_meta("vault_verifier")?
        .ok_or_else(|| AppError::NotFound("vault".into()))?;
    let dek = derive_dek(pass, &salt)?;
    let parts: Vec<&str> = verifier.split(':').collect();
    if parts.len() != 2 {
        return Err(AppError::Crypto("bad verifier".into()));
    }
    let nonce = hex::decode(parts[0]).map_err(|e| AppError::Crypto(e.to_string()))?;
    let ct = hex::decode(parts[1]).map_err(|e| AppError::Crypto(e.to_string()))?;
    let cipher = cipher_from(&dek);
    cipher
        .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
        .map_err(|_| AppError::BadPassphrase)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fresh_db() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("aevora_vault_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = db::init(&dir.join("v.db"));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        fresh_db();
        // may already be initialized in same process; use unique meta keys via re-init path
        if !is_initialized().unwrap() {
            unlock_or_create("test-pass-123").unwrap();
        } else if !is_unlocked().unwrap() {
            unlock_or_create("test-pass-123").unwrap();
        }
        let (n, c) = encrypt(b"secret-key-material").unwrap();
        let pt = decrypt(&n, &c).unwrap();
        assert_eq!(pt, b"secret-key-material");
    }

    #[test]
    fn wrong_passphrase_fails() {
        fresh_db();
        if !is_initialized().unwrap() {
            unlock_or_create("correct-horse-1").unwrap();
        } else if !is_unlocked().unwrap() {
            unlock_or_create("correct-horse-1").unwrap();
        }
        lock().unwrap();
        let err = unlock_or_create("wrong-pass-xxx");
        assert!(err.is_err());
        // restore
        let _ = unlock_or_create("correct-horse-1");
    }
}
