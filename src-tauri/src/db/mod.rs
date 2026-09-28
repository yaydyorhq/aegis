pub mod schema;

use crate::error::{AppError, AppResult};
use rusqlite::Connection;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static DB: OnceLock<Mutex<Connection>> = OnceLock::new();

pub fn init(db_path: &PathBuf) -> AppResult<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(schema::SCHEMA)?;
    migrate(&conn)?;
    seed_chains(&conn)?;
    let _ = DB.set(Mutex::new(conn));
    Ok(())
}

/// Lightweight migrations for existing installs (no migration framework).
/// Must run AFTER SCHEMA batch (tables exist) but the group index is created
/// only here — putting it in SCHEMA would crash old DBs where `group_id`
/// does not exist yet when CREATE INDEX runs.
fn migrate(conn: &Connection) -> AppResult<()> {
    // wallets.group_id — added for wallet groups feature
    ensure_column(
        conn,
        "wallets",
        "group_id",
        "INTEGER REFERENCES wallet_groups(id) ON DELETE SET NULL",
    )?;
    // Safe for both fresh + upgraded DBs (column is guaranteed present now)
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_wallets_group ON wallets(group_id)",
        [],
    )?;

    // mint_tasks — Create Task advanced fields
    ensure_column(conn, "mint_tasks", "function_name", "TEXT")?;
    ensure_column(conn, "mint_tasks", "is_hex", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "mint_tasks", "parameters", "TEXT")?;
    ensure_column(conn, "mint_tasks", "rpc_endpoints", "TEXT")?;
    ensure_column(conn, "mint_tasks", "flashbots", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "mint_tasks", "gas_limit", "TEXT")?;
    ensure_column(conn, "mint_tasks", "max_fee_gwei", "TEXT")?;
    ensure_column(conn, "mint_tasks", "priority_fee_gwei", "TEXT")?;
    ensure_column(conn, "mint_tasks", "nonce_override", "TEXT")?;
    ensure_column(conn, "mint_tasks", "scheduled_at", "INTEGER")?;
    ensure_column(conn, "mint_tasks", "delay_ms", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(conn, "mint_tasks", "mode", "TEXT NOT NULL DEFAULT 'execute'")?;
    ensure_column(conn, "nft_cache", "opensea_url", "TEXT")?;
    ensure_column(
        conn,
        "mint_tasks",
        "poll_attempts",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "mint_tasks",
        "auto_retries",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // Fire-time OpenSea mint data: JSON {collection, token_id} — the task
    // resolves SIWE + stage + MintAction when it fires instead of at enqueue.
    ensure_column(conn, "mint_tasks", "opensea_ref", "TEXT")?;
    Ok(())
}

/// SQLite identifiers (table/column) interpolated into raw SQL.
/// Only `[A-Za-z0-9_]` up to 64 chars — a future caller can never smuggle
/// user input into `PRAGMA table_info()` / `ALTER TABLE` via these params.
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Column declarations come from hardcoded callers, but still guard them:
/// no `;`, `--`, backticks or double quotes (comments / statement stacking).
fn valid_decl(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 160
        && s.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, ' ' | '_' | ',' | '(' | ')' | '\'' | '.')
        })
        && !s.contains("--")
        && !s.contains(';')
}

fn ensure_column(conn: &Connection, table: &str, name: &str, decl: &str) -> AppResult<()> {
    if !valid_ident(table) || !valid_ident(name) {
        return Err(AppError::Other(format!(
            "unsafe SQL identifier: {table}.{name}"
        )));
    }
    if !valid_decl(decl) {
        return Err(AppError::Other(format!(
            "unsafe column declaration for {name}"
        )));
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    let mut found = false;
    while let Some(row) = rows.next()? {
        let col: String = row.get(1)?;
        if col == name {
            found = true;
            break;
        }
    }
    drop(rows);
    drop(stmt);
    if !found {
        conn.execute(
            &format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"),
            [],
        )?;
    }
    Ok(())
}

pub fn with_conn<F, T>(f: F) -> AppResult<T>
where
    F: FnOnce(&Connection) -> AppResult<T>,
{
    let guard = DB
        .get()
        .ok_or_else(|| AppError::Other("db not initialized".into()))?
        .lock()
        .map_err(|_| AppError::Other("db lock poisoned".into()))?;
    f(&guard)
}

fn seed_chains(conn: &Connection) -> AppResult<()> {
    // Insert missing defaults (INSERT OR IGNORE by chain_id UNIQUE) so new
    // default chains appear on existing installs without wiping user rows.
    for (name, chain_id, rpc, symbol, explorer) in schema::DEFAULT_CHAINS {
        conn.execute(
            "INSERT OR IGNORE INTO chains(name, chain_id, rpc_url, symbol, explorer, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![name, chain_id, rpc, symbol, explorer],
        )?;
    }
    // Refresh retired default RPCs only when the row still points at the
    // known-dead URL (user-customized endpoints are left alone).
    for (chain_id, retired) in schema::RETIRED_DEFAULT_RPCS {
        let default = schema::DEFAULT_CHAINS
            .iter()
            .find(|c| c.1 == *chain_id)
            .map(|c| c.2)
            .ok_or_else(|| AppError::Other(format!("no default for chain {chain_id}")))?;
        for old in *retired {
            conn.execute(
                "UPDATE chains SET rpc_url = ?1 WHERE chain_id = ?2 AND rpc_url = ?3",
                rusqlite::params![default, chain_id, old],
            )?;
        }
    }
    // One-time: disable former stock defaults no longer in DEFAULT_CHAINS so
    // existing installs match the curated set. Re-enable is free afterward.
    const PRUNE_KEY: &str = "chains.pruned_removed_defaults";
    let pruned: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [PRUNE_KEY], |r| r.get(0))
        .ok();
    if pruned.is_none() {
        for id in schema::REMOVED_DEFAULT_CHAIN_IDS {
            conn.execute(
                "UPDATE chains SET enabled = 0 WHERE chain_id = ?1 AND enabled != 0",
                [*id],
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, '1')
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            [PRUNE_KEY],
        )?;
    }
    Ok(())
}

/// `SystemTime::now().duration_since(UNIX_EPOCH)` with an honest fallback:
/// if the clock sits before 1970 we report *negative* milliseconds instead
/// of collapsing to epoch 0.  Epoch 0 would freeze every `scheduled_at` gate
/// (`0 < scheduled_at` never fires → queued mints silently stall forever).
fn millis_since_epoch(
    since: Result<std::time::Duration, std::time::SystemTimeError>,
) -> i64 {
    match since {
        Ok(d) => d.as_millis() as i64,
        Err(e) => -(e.duration().as_millis() as i64),
    }
}

pub fn now_ms() -> i64 {
    millis_since_epoch(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
}

pub fn meta_get(key: &str) -> AppResult<Option<String>> {
    with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
        let mut rows = stmt.query([key])?;
        match rows.next()? {
            Some(r) => Ok(r.get::<_, String>(0).ok()),
            None => Ok(None),
        }
    })
}

pub fn meta_set(key: &str, value: &str) -> AppResult<()> {
    with_conn(|conn| {
        conn.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn init_creates_tables() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_test_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        // Use a fresh connection path unique per test; global OnceLock only allows one init per process,
        // so we only assert schema SQL applies cleanly on a local connection.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::SCHEMA).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
             ('wallets','chains','activity','mint_tasks','eligibility_checks','nft_cache','api_keys','pnl_scans','wallet_groups','fund_jobs','fund_txs')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 11);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn seed_refreshes_retired_rpcs_keeps_custom() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_seed_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("seed.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::SCHEMA).unwrap();
        // Simulate old install with retired defaults + one user override.
        conn.execute(
            "INSERT INTO chains(name, chain_id, rpc_url, symbol, explorer, enabled)
             VALUES ('Ethereum', 1, 'https://eth.llamarpc.com', 'ETH', NULL, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO chains(name, chain_id, rpc_url, symbol, explorer, enabled)
             VALUES ('Optimism', 10, 'https://user-custom.example/rpc', 'OP', NULL, 1)",
            [],
        )
        .unwrap();
        seed_chains(&conn).unwrap();
        let eth_rpc: String = conn
            .query_row("SELECT rpc_url FROM chains WHERE chain_id = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            eth_rpc,
            "https://cloudflare-eth.com",
            "retired eth default should refresh"
        );
        let bsc_rpc: String = conn
            .query_row("SELECT rpc_url FROM chains WHERE chain_id = 56", [], |r| r.get(0))
            .unwrap();
        assert_eq!(bsc_rpc, "https://bsc.meowrpc.com");
        // Former default (Optimism) was pre-seeded with a custom RPC: kept,
        // but disabled by the one-time prune so only curated defaults show.
        let (op_rpc, op_enabled): (String, i64) = conn
            .query_row(
                "SELECT rpc_url, enabled FROM chains WHERE chain_id = 10",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(op_rpc, "https://user-custom.example/rpc", "custom RPC kept");
        assert_eq!(op_enabled, 0, "removed default disabled once");
        // All curated defaults present and enabled.
        let enabled: i64 = conn
            .query_row("SELECT COUNT(*) FROM chains WHERE enabled = 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            enabled,
            schema::DEFAULT_CHAINS.len() as i64,
            "only curated defaults enabled"
        );
        // Re-running seed must not re-disable if the user re-enabled later.
        conn.execute("UPDATE chains SET enabled = 1 WHERE chain_id = 10", [])
            .unwrap();
        seed_chains(&conn).unwrap();
        let op_enabled2: i64 = conn
            .query_row("SELECT enabled FROM chains WHERE chain_id = 10", [], |r| r.get(0))
            .unwrap();
        assert_eq!(op_enabled2, 1, "prune is one-time only");
        let _ = std::fs::remove_dir_all(dir);
    }
}
