use crate::db;
use crate::error::{AppError, AppResult};
use aes_gcm::aead::{Aead, KeyInit, OsRng as AeadOsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use argon2::password_hash::{PasswordHasher, SaltString};
use argon2::Argon2;
use once_cell::sync::OnceCell;
use rand::RngCore;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 12;

/// Consecutive-failure gate: after this many wrong passphrases the next
/// attempt — right or wrong — waits out the cooldown. Argon2id remains the
/// real cost; this just slows a local interactive brute force to a crawl.
const MAX_FAILED_UNLOCK: u32 = 5;
const UNLOCK_COOLDOWN: Duration = Duration::from_secs(30);

fn attempt_state() -> std::sync::MutexGuard<'static, (u32, Option<Instant>)> {
    static STATE: once_cell::sync::Lazy<std::sync::Mutex<(u32, Option<Instant>)>> =
        once_cell::sync::Lazy::new(|| std::sync::Mutex::new((0, None)));
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn gate_attempts() -> AppResult<()> {
    let mut st = attempt_state();
    if let Some(t) = st.1 {
        let left = UNLOCK_COOLDOWN.saturating_sub(t.elapsed());
        if left > Duration::ZERO {
            return Err(AppError::Other(format!(
                "too many failed passphrase attempts — retry in {}s",
                left.as_secs() + 1
            )));
        }
        *st = (0, None);
    }
    Ok(())
}

fn note_attempt(ok: bool) {
    let mut st = attempt_state();
    if ok {
        *st = (0, None);
    } else {
        st.0 += 1;
        if st.0 >= MAX_FAILED_UNLOCK {
            st.1 = Some(Instant::now());
        }
    }
}

#[cfg(test)]
pub fn note_attempt_reset() {
    *attempt_state() = (0, None);
}

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
    gate_attempts()?;
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
        if nonce.len() != NONCE_LEN {
            return Err(AppError::Crypto("bad verifier nonce".into()));
        }
        let cipher = cipher_from(&dek);
        let plain = cipher
            .decrypt(Nonce::from_slice(&nonce), ct.as_ref())
            .map_err(|_| {
                note_attempt(false);
                AppError::BadPassphrase
            })?;
        if plain != b"aegis-vault-v1" {
            note_attempt(false);
            return Err(AppError::BadPassphrase);
        }
        note_attempt(true);
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
        .encrypt(Nonce::from_slice(&nonce), b"aegis-vault-v1".as_ref())
        .map_err(|e| AppError::Crypto(e.to_string()))?;
    set_meta(
        "vault_verifier",
        &format!("{}:{}", hex::encode(nonce), hex::encode(ct)),
    )?;
    note_attempt(true);
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
    gate_attempts()?;
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
    if nonce.len() != NONCE_LEN {
        return Err(AppError::Crypto("bad verifier nonce".into()));
    }
    let cipher = cipher_from(&dek);
    match cipher.decrypt(Nonce::from_slice(&nonce), ct.as_ref()) {
        Ok(_) => {
            note_attempt(true);
            Ok(())
        }
        Err(_) => {
            note_attempt(false);
            Err(AppError::BadPassphrase)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fresh_db() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_vault_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = db::init(&dir.join("v.db"));
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let _serial = crate::db::test_support::serial_guard();
        fresh_db();
        // may already be initialized in same process; use unique meta keys via re-init path
        if !is_unlocked().unwrap() {
            unlock_or_create("test-pass-123").unwrap();
        }
        let (n, c) = encrypt(b"secret-key-material").unwrap();
        let pt = decrypt(&n, &c).unwrap();
        assert_eq!(pt, b"secret-key-material");
    }

    #[test]
    fn wrong_passphrase_fails() {
        let _serial = crate::db::test_support::serial_guard();
        fresh_db();
        if !is_unlocked().unwrap() {
            unlock_or_create("correct-horse-1").unwrap();
        }
        // Do NOT call lock() here — tests run in parallel and locking the
        // process-global vault would race with mint e2e tests mid-run.
        // unlock_or_create still fails: the stored verifier rejects the pass
        // before any vault state is touched.
        let err = unlock_or_create("wrong-pass-xxx");
        assert!(err.is_err());
        assert!(confirm_passphrase("wrong-pass-xxx").is_err());
        assert!(is_unlocked().unwrap(), "vault must stay unlocked for other tests");
    }

    /// Five consecutive wrong passphrases trip a cooldown that refuses even
    /// the CORRECT passphrase until it expires — the interactive brute-force
    /// brake. State resets between tests via note_attempt_reset.
    #[test]
    fn unlock_locks_out_after_repeated_failures() {
        let _serial = crate::db::test_support::serial_guard();
        fresh_db();
        note_attempt_reset();
        // DB is process-global: try the passphrases other tests use and
        // remember the one that actually opened this vault.
        let mut unlocked_with: Option<String> = None;
        if !is_unlocked().unwrap() {
            for pass in [
                "aegis-lockout-pass",
                "test-pass-123",
                "correct-horse-1",
                "aegis-e2e-pass",
            ] {
                if unlock_or_create(pass).is_ok() {
                    unlocked_with = Some(pass.to_string());
                    break;
                }
            }
            assert!(unlocked_with.is_some(), "could not unlock vault for lockout test");
        }
        for _ in 0..MAX_FAILED_UNLOCK {
            assert!(unlock_or_create("totally-wrong-pass").is_err());
        }
        // Cooldown active: even the correct passphrase is refused.
        let probe = unlocked_with.clone().unwrap_or_else(|| "aegis-lockout-pass".into());
        let err = unlock_or_create(&probe).unwrap_err();
        assert!(err.to_string().contains("retry in"), "{err}");
        assert!(confirm_passphrase(&probe).is_err());
        note_attempt_reset();
        match unlock_or_create(&probe) {
            Ok(_) => {}
            // Vault was unlocked by another test under an unknown passphrase —
            // the gate must at least be lifted (error ≠ cooldown).
            Err(e) => assert!(
                !e.to_string().contains("retry in"),
                "cooldown should have lifted: {e}"
            ),
        }
    }
}
