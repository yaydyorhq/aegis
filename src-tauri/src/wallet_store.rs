use crate::db;
use crate::error::{AppError, AppResult};
use crate::vault;
use crate::wallet;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

#[derive(Serialize)]
pub struct WalletRow {
    pub id: i64,
    pub label: String,
    pub address: String,
    pub created_at: i64,
    pub group_id: Option<i64>,
}

#[derive(Serialize)]
pub struct GroupRow {
    pub id: i64,
    pub name: String,
    pub created_at: i64,
    pub wallet_count: i64,
}

#[derive(Deserialize)]
pub struct BulkImportItem {
    pub label: Option<String>,
    pub private_key: String,
}

#[derive(Serialize)]
pub struct BulkImportResultItem {
    pub index: usize,
    pub status: String, // imported | duplicate | invalid
    pub address: Option<String>,
    pub label: Option<String>,
    pub error: Option<String>,
}

const WALLET_COLS: &str = "id, label, address, created_at, group_id";

pub fn log_activity(kind: &str, summary: &str, payload: Option<&str>, ok: bool) {
    // Choke point: every caller formats raw RPC/endpoint URLs into summaries,
    // and those URLs can carry API keys in the path. Redact before persisting.
    let summary = crate::chain::redact_urls_in(summary);
    let payload = payload.map(crate::chain::redact_urls_in);
    let _ = db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO activity(kind, summary, payload, ok, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![kind, summary, payload, ok as i64, db::now_ms()],
        )?;
        Ok(())
    });
}

fn map_wallet(r: &rusqlite::Row<'_>) -> rusqlite::Result<WalletRow> {
    Ok(WalletRow {
        id: r.get(0)?,
        label: r.get(1)?,
        address: r.get(2)?,
        created_at: r.get(3)?,
        group_id: r.get(4)?,
    })
}

pub fn list_wallets() -> AppResult<Vec<WalletRow>> {
    db::with_conn(|conn| {
        let sql = format!("SELECT {WALLET_COLS} FROM wallets ORDER BY created_at DESC");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_wallet)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn create_wallet(label: &str, private_key: Option<&str>) -> AppResult<WalletRow> {
    create_wallet_in_group(label, private_key, None)
}

pub fn create_wallet_in_group(
    label: &str,
    private_key: Option<&str>,
    group_id: Option<i64>,
) -> AppResult<WalletRow> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let new = match private_key {
        Some(pk) => wallet::parse_import(label, pk)?,
        None => wallet::generate(label)?,
    };
    let address = wallet::to_checksum_str(&new.address);
    if address_exists(&address)? {
        return Err(AppError::Invalid(format!(
            "wallet already exists: {address}"
        )));
    }
    let (nonce, ct) = wallet::encrypt_privkey(&new.private_key)?;
    let created = db::now_ms();
    let id = db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO wallets(label, address, enc_privkey, nonce, created_at, group_id)
             VALUES (?1,?2,?3,?4,?5,?6)",
            rusqlite::params![new.label, address, ct, nonce, created, group_id],
        )?;
        Ok(conn.last_insert_rowid())
    })?;
    log_activity(
        "wallet.create",
        &format!("Created wallet `#{id}` {} {}", new.label, address),
        None,
        true,
    );
    Ok(WalletRow {
        id,
        label: new.label,
        address,
        created_at: created,
        group_id,
    })
}

pub fn delete_wallet(id: i64) -> AppResult<()> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
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
        let sql = format!("SELECT {WALLET_COLS} FROM wallets WHERE id = ?1");
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row([id], map_wallet)
            .map_err(|_| AppError::NotFound(format!("wallet {id}")))
    })
}

pub fn rename_wallet(id: i64, label: &str) -> AppResult<WalletRow> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    let label = label.trim();
    if label.is_empty() {
        return Err(AppError::Invalid("label required".into()));
    }
    if label.len() > 128 {
        return Err(AppError::Invalid("label max 128 chars".into()));
    }
    let n = db::with_conn(|conn| {
        conn.execute(
            "UPDATE wallets SET label = ?1 WHERE id = ?2",
            rusqlite::params![label, id],
        )
        .map_err(AppError::Db)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("wallet {id}")));
    }
    log_activity(
        "wallet.rename",
        &format!("Renamed wallet #{id} to `{label}`"),
        None,
        true,
    );
    get_wallet(id)
}

fn address_exists(address: &str) -> AppResult<bool> {
    db::with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT 1 FROM wallets WHERE address = ?1 LIMIT 1")?;
        let found = stmt.exists([address])?;
        Ok(found)
    })
}

// ── Groups ──────────────────────────────────────────────────────────

pub fn list_groups() -> AppResult<Vec<GroupRow>> {
    db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT g.id, g.name, g.created_at,
                    (SELECT COUNT(*) FROM wallets w WHERE w.group_id = g.id)
             FROM wallet_groups g ORDER BY g.created_at ASC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(GroupRow {
                id: r.get(0)?,
                name: r.get(1)?,
                created_at: r.get(2)?,
                wallet_count: r.get(3)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn create_group(name: &str) -> AppResult<GroupRow> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("group name required".into()));
    }
    if name.len() > 64 {
        return Err(AppError::Invalid("group name max 64 chars".into()));
    }
    let created = db::now_ms();
    let id = db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO wallet_groups(name, created_at) VALUES (?1, ?2)",
            rusqlite::params![name, created],
        )
        .map_err(|e| match &e {
            rusqlite::Error::SqliteFailure(fi, _)
                if fi.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                AppError::Invalid(format!("group `{name}` already exists"))
            }
            _ => AppError::Db(e),
        })?;
        Ok(conn.last_insert_rowid())
    })?;
    log_activity("wallet.group.create", &format!("Created group `{name}`"), None, true);
    Ok(GroupRow { id, name: name.to_string(), created_at: created, wallet_count: 0 })
}

pub fn rename_group(id: i64, name: &str) -> AppResult<GroupRow> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("group name required".into()));
    }
    if name.len() > 64 {
        return Err(AppError::Invalid("group name max 64 chars".into()));
    }
    let n = db::with_conn(|conn| {
        conn.execute(
            "UPDATE wallet_groups SET name = ?1 WHERE id = ?2",
            rusqlite::params![name, id],
        )
        .map_err(|e| match &e {
            rusqlite::Error::SqliteFailure(fi, _)
                if fi.code == rusqlite::ErrorCode::ConstraintViolation =>
            {
                AppError::Invalid(format!("group `{name}` already exists"))
            }
            _ => AppError::Db(e),
        })
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("group {id}")));
    }
    log_activity("wallet.group.rename", &format!("Renamed group #{id} to `{name}`"), None, true);
    list_groups()?.into_iter().find(|g| g.id == id)
        .ok_or_else(|| AppError::NotFound(format!("group {id}")))
}

pub fn delete_group(id: i64) -> AppResult<()> {
    db::with_conn(|conn| {
        // Explicitly unassign first (belt-and-suspenders beyond ON DELETE SET NULL)
        conn.execute("UPDATE wallets SET group_id = NULL WHERE group_id = ?1", [id])?;
        let n = conn.execute("DELETE FROM wallet_groups WHERE id = ?1", [id])?;
        if n == 0 {
            return Err(AppError::NotFound(format!("group {id}")));
        }
        Ok(())
    })?;
    log_activity("wallet.group.delete", &format!("Deleted group #{id}"), None, true);
    Ok(())
}

pub fn set_wallet_group(wallet_id: i64, group_id: Option<i64>) -> AppResult<()> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    if let Some(gid) = group_id {
        db::with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM wallet_groups WHERE id = ?1")?;
            if !stmt.exists([gid])? {
                return Err(AppError::NotFound(format!("group {gid}")));
            }
            Ok(())
        })?;
    }
    let n = db::with_conn(|conn| {
        conn.execute(
            "UPDATE wallets SET group_id = ?1 WHERE id = ?2",
            rusqlite::params![group_id, wallet_id],
        )
        .map_err(AppError::Db)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("wallet {wallet_id}")));
    }
    log_activity(
        "wallet.group.assign",
        &format!("Wallet #{wallet_id} → group {}", group_id.map(|g| g.to_string()).unwrap_or_else(|| "none".into())),
        None,
        true,
    );
    Ok(())
}

/// Error-tolerant bulk import: each item succeeds or fails independently.
pub fn import_bulk(
    items: &[BulkImportItem],
    group_id: Option<i64>,
) -> AppResult<Vec<BulkImportResultItem>> {
    if !vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    if items.is_empty() {
        return Err(AppError::Invalid("no keys provided".into()));
    }
    if items.len() > 500 {
        return Err(AppError::Invalid("max 500 keys per import".into()));
    }
    if let Some(gid) = group_id {
        db::with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM wallet_groups WHERE id = ?1")?;
            if !stmt.exists([gid])? {
                return Err(AppError::NotFound(format!("group {gid}")));
            }
            Ok(())
        })?;
    }

    let mut results = Vec::with_capacity(items.len());
    let mut imported = 0usize;
    let mut duplicates = 0usize;
    let mut invalid = 0usize;

    for (i, item) in items.iter().enumerate() {
        let fallback_label = format!("import-{:03}", i + 1);
        let label = item
            .label
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&fallback_label);

        let outcome = (|| -> AppResult<(String, String)> {
            let new = wallet::parse_import(label, &item.private_key)?;
            let address = wallet::to_checksum_str(&new.address);
            if address_exists(&address)? {
                return Err(AppError::Invalid(format!("duplicate: {address}")));
            }
            let (nonce, ct) = wallet::encrypt_privkey(&new.private_key)?;
            let created = db::now_ms();
            db::with_conn(|conn| {
                conn.execute(
                    "INSERT INTO wallets(label, address, enc_privkey, nonce, created_at, group_id)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    rusqlite::params![label, address, ct, nonce, created, group_id],
                )?;
                Ok(())
            })?;
            Ok((address, label.to_string()))
        })();

        match outcome {
            Ok((address, lbl)) => {
                imported += 1;
                results.push(BulkImportResultItem {
                    index: i,
                    status: "imported".into(),
                    address: Some(address),
                    label: Some(lbl),
                    error: None,
                });
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("duplicate") {
                    duplicates += 1;
                    results.push(BulkImportResultItem {
                        index: i,
                        status: "duplicate".into(),
                        address: None,
                        label: Some(label.to_string()),
                        error: Some(msg),
                    });
                } else {
                    invalid += 1;
                    results.push(BulkImportResultItem {
                        index: i,
                        status: "invalid".into(),
                        address: None,
                        label: Some(label.to_string()),
                        error: Some(msg),
                    });
                }
            }
        }
    }

    log_activity(
        "wallet.import_bulk",
        &format!("Bulk import: {imported} imported, {duplicates} duplicates, {invalid} invalid"),
        None,
        imported > 0,
    );
    Ok(results)
}
