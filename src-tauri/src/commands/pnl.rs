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
    persist_collection_scan(&result)?;
    Ok(result)
}

fn persist_collection_scan(
    result: &collection_pnl::CollectionPnlResult,
) -> AppResult<()> {
    let payload = serde_json::to_string(result)
        .map_err(|e| crate::error::AppError::Other(e.to_string()))?;
    let contract_lc = result.contract.to_lowercase();
    crate::db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO collection_pnl_scans(contract, chain_id, payload, scanned_at)
             VALUES (?1,?2,?3,?4)",
            rusqlite::params![contract_lc, result.chain_id, payload, result.scanned_at],
        )?;
        Ok(())
    })
}

/// Re-run the LAST Collection PnL scan with identical parameters (contract,
/// wallets, window, fee) — the engine behind the Settings "Portfolio auto
/// re-scan" scheduler. `Ok(None)` when there is nothing to repeat yet.
#[tauri::command]
pub async fn collection_pnl_rescan() -> AppResult<Option<collection_pnl::CollectionPnlResult>> {
    let Some(prev) = collection_pnl_last()? else {
        return Ok(None);
    };
    // Rebuild the exact target list (wallets + externals) from the persisted rows.
    let mut rebuilt: Vec<collection_pnl::Target> = Vec::new();
    for r in &prev.rows {
        rebuilt.push(collection_pnl::Target {
            wallet_id: r.wallet_id,
            label: r.label.clone(),
            address: r.address.clone(),
        });
    }
    if rebuilt.is_empty() {
        return Ok(None);
    }
    let result = collection_pnl::scan(
        &prev.contract,
        prev.chain_id,
        rebuilt,
        prev.window_blocks,
        prev.fee_bps,
        &|_, _, _| {},
    )
    .await?;
    persist_collection_scan(&result)?;
    Ok(Some(result))
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

/// One point of the dashboard net-worth sparkline: a past Collection PnL scan.
#[derive(serde::Serialize)]
pub struct CollectionPnlPoint {
    pub scanned_at: i64,
    pub net_eth: Option<String>,
    pub roi_pct: Option<f64>,
}

/// Recent Collection PnL scans, oldest → newest — the net-worth trail for the
/// dashboard sparkline. Only scans with a computable net carry `net_eth`.
#[tauri::command]
pub fn collection_pnl_history(limit: Option<u32>) -> AppResult<Vec<CollectionPnlPoint>> {
    let lim = limit.unwrap_or(30).min(200);
    let payloads: Vec<String> = crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT payload FROM collection_pnl_scans ORDER BY scanned_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([lim], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let mut out = Vec::with_capacity(payloads.len());
    for p in payloads {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&p) else {
            continue;
        };
        out.push(CollectionPnlPoint {
            scanned_at: v.get("scanned_at").and_then(|x| x.as_i64()).unwrap_or(0),
            net_eth: v
                .pointer("/totals/net_eth")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            roi_pct: v.pointer("/totals/roi_pct").and_then(|x| x.as_f64()),
        });
    }
    out.reverse();
    Ok(out)
}
