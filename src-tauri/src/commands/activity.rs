use crate::error::AppResult;
use serde::Serialize;

#[derive(Serialize)]
pub struct ActivityRow {
    pub id: i64,
    pub kind: String,
    pub summary: String,
    pub payload: Option<String>,
    pub ok: i64,
    pub created_at: i64,
}

#[tauri::command]
pub fn activity_list(limit: Option<u32>, kind: Option<String>) -> AppResult<Vec<ActivityRow>> {
    let lim = limit.unwrap_or(200);
    crate::db::with_conn(|conn| {
        let mut out = Vec::new();
        if let Some(k) = kind {
            let mut stmt = conn.prepare(
                "SELECT id, kind, summary, payload, ok, created_at
                 FROM activity WHERE kind LIKE ?1 ORDER BY created_at DESC LIMIT ?2",
            )?;
            let rows = stmt.query_map(rusqlite::params![format!("{k}%"), lim], map)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, kind, summary, payload, ok, created_at
                 FROM activity ORDER BY created_at DESC LIMIT ?1",
            )?;
            let rows = stmt.query_map([lim], map)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    })
}

fn map(r: &rusqlite::Row) -> rusqlite::Result<ActivityRow> {
    Ok(ActivityRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        summary: r.get(2)?,
        payload: r.get(3)?,
        ok: r.get(4)?,
        created_at: r.get(5)?,
    })
}
