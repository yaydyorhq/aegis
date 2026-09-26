use crate::db;
use crate::error::{AppError, AppResult};
use crate::vault;
use crate::wallet;
use serde::Serialize;
use zeroize::Zeroizing;

#[derive(Serialize)]
pub struct WalletRow {
    pub id: i64,
    pub label: String,
    pub address: String,
    pub created_at: i64,
}

pub fn log_activity(kind: &str, summary: &str, payload: Option<&str>, ok: bool) {
    let _ = db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO activity(kind, summary, payload, ok, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![kind, summary, payload, ok as i64, db::now_ms()],
        )?;
        Ok(())
    });
}

pub fn list_wallets() -> AppResult<Vec<WalletRow>> {
    db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, label, address, created_at FROM wallets ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(WalletRow {
                id: r.get(0)?,
                label: r.get(1)?,
                address: r.get(2)?,
                created_at: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn create_wallet(label: &str, private_key: Option<&str>) -> AppResult<WalletRow> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let new = match private_key {
        Some(pk) => wallet::parse_import(label, pk)?,
        None => wallet::generate(label)?,
    };
    let address = wallet::to_checksum_str(&new.address);
    let (nonce, ct) = wallet::encrypt_privkey(&new.private_key)?;
    let created = db::now_ms();
    db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO wallets(label, address, enc_privkey, nonce, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![new.label, address, ct, nonce, created],
        )?;
        Ok(())
    })?;
    let _ = address;
    log_activity(
        "wallet.create",
        &format!("Created wallet `{}` {}", new.label, wallet::to_checksum_str(&new.address)),
        None,
        true,
    );
    let id = db::with_conn(|conn| Ok(conn.last_insert_rowid()))?;
    Ok(WalletRow {
        id,
        label: new.label,
        address,
        created_at: created,
    })
}

pub fn delete_wallet(id: i64) -> AppResult<()> {
    let n = db::with_conn(|conn| {
        Ok(conn.execute("DELETE FROM wallets WHERE id = ?1", [id])?)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("wallet {id}")));
    }
    log_activity("wallet.delete", &format!("Deleted wallet #{id}"), None, true);
    Ok(())
}

pub fn export_private_key(id: i64, pass_confirm: &str) -> AppResult<String> {
    vault::confirm_passphrase(pass_confirm)?;
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let (nonce, ct) = db::with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT nonce, enc_privkey FROM wallets WHERE id = ?1")?;
        let row: (Vec<u8>, Vec<u8>) = stmt.query_row([id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(row)
    })?;
    let sk: Zeroizing<[u8; 32]> = wallet::decrypt_privkey(&nonce, &ct)?;
    log_activity("wallet.export", &format!("Exported private key for wallet #{id}"), None, true);
    Ok(format!("0x{}", hex::encode(sk.as_ref())))
}

/// Internal: get encrypted key material for signing flows.
pub fn key_material(wallet_id: i64) -> AppResult<(Vec<u8>, Vec<u8>)> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    db::with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT nonce, enc_privkey FROM wallets WHERE id = ?1")?;
        let row: (Vec<u8>, Vec<u8>) =
            stmt.query_row([wallet_id], |r| Ok((r.get(0)?, r.get(1)?)))
                .map_err(|_| AppError::NotFound(format!("wallet {wallet_id}")))?;
        Ok(row)
    })
}

pub fn get_wallet(id: i64) -> AppResult<WalletRow> {
    db::with_conn(|conn| {
        let mut stmt =
            conn.prepare("SELECT id, label, address, created_at FROM wallets WHERE id = ?1")?;
        stmt.query_row([id], |r| {
            Ok(WalletRow {
                id: r.get(0)?,
                label: r.get(1)?,
                address: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(|_| AppError::NotFound(format!("wallet {id}")))
    })
}
