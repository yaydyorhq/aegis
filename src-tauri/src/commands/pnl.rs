use crate::collection_pnl;
use crate::error::AppResult;
use crate::pnl::{self, PnlHistoryRow, PnlLastScan, PnlScanResult};
use tauri::Emitter;

#[tauri::command]
pub async fn pnl_scan(
    wallet_id: i64,
    chain_id: i64,
    window_blocks: Option<u64>,
) -> AppResult<PnlScanResult> {
    pnl::scan_pnl(wallet_id, chain_id, window_blocks.unwrap_or(50_000)).await
}

#[tauri::command]
pub fn pnl_last_scan(wallet_id: i64) -> AppResult<Option<PnlLastScan>> {
    pnl::last_scan(wallet_id)
}

#[tauri::command]
pub fn pnl_history(wallet_id: i64, limit: Option<u64>) -> AppResult<Vec<PnlHistoryRow>> {
    pnl::history(wallet_id, limit.unwrap_or(20))
}

#[tauri::command]
pub async fn collection_pnl_scan(
    app: tauri::AppHandle,
    contract: String,
    chain_id: i64,
    wallet_ids: Option<Vec<i64>>,
    extra_addresses: Option<Vec<String>>,
    window_blocks: Option<u64>,
    fee_bps: Option<u64>,
) -> AppResult<collection_pnl::CollectionPnlResult> {
    let targets = collection_pnl::resolve_targets(wallet_ids, extra_addresses)?;
    let progress_app = app.clone();
    let on_progress = move |phase: &str, done: u64, total: u64| {
        let _ = progress_app.emit(
            "collection-pnl-progress",
            serde_json::json!({ "phase": phase, "done": done, "total": total }),
        );
    };
    let result = collection_pnl::scan(
        &contract,
        chain_id,
        targets,
        window_blocks.unwrap_or(0),
        fee_bps.unwrap_or(250),
        &on_progress,
    )
    .await?;

    // Persist for the Dashboard "Portfolio performance" panel.
    let payload = serde_json::to_string(&result)
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    let scanned_at = result.scanned_at;
    let contract_lc = result.contract.to_lowercase();
    crate::db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO collection_pnl_scans(contract, chain_id, payload, scanned_at)
             VALUES (?1,?2,?3,?4)",
            rusqlite::params![contract_lc, result.chain_id, payload, scanned_at],
        )?;
        Ok(())
    })?;
    Ok(result)
}

#[tauri::command]
pub fn collection_pnl_last() -> AppResult<Option<collection_pnl::CollectionPnlResult>> {
    let payload: Option<String> = crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT payload FROM collection_pnl_scans ORDER BY scanned_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(r) => Ok(Some(r.get(0)?)),
            None => Ok(None),
        }
    })?;
    payload
        .map(|p| serde_json::from_str(&p).map_err(|e| crate::error::AppError::Other(e.to_string())))
        .transpose()
}
