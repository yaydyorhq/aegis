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
    seed_chains(&conn)?;
    let _ = DB.set(Mutex::new(conn));
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
    let mut stmt = conn.prepare("SELECT COUNT(*) FROM chains")?;
    let count: i64 = stmt.query_row([], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }
    for (name, chain_id, rpc, symbol, explorer) in schema::DEFAULT_CHAINS {
        conn.execute(
            "INSERT OR IGNORE INTO chains(name, chain_id, rpc_url, symbol, explorer, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5, 1)",
            rusqlite::params![name, chain_id, rpc, symbol, explorer],
        )?;
    }
    Ok(())
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn init_creates_tables() {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("aevora_test_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.db");
        // Use a fresh connection path unique per test; global OnceLock only allows one init per process,
        // so we only assert schema SQL applies cleanly on a local connection.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(schema::SCHEMA).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN
                 ('wallets','chains','activity','mint_tasks','eligibility_checks','nft_cache','api_keys','pnl_scans')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 8);
        let _ = std::fs::remove_dir_all(dir);
    }
}
