use crate::chain::eligibility;
use crate::error::AppResult;
use crate::opensea;
use crate::wallet_store;
use serde::Serialize;

#[derive(Serialize)]
pub struct EligibilityResult {
    pub wallet_id: Option<i64>,
    pub address: String,
    pub collection: String,
    pub eligible: Option<bool>,
    pub detail: String,
    pub checked_at: i64,
}

#[tauri::command]
pub async fn eligibility_run(
    wallet_ids: Vec<i64>,
    chain_id: i64,
    collection: String,
) -> AppResult<Vec<EligibilityResult>> {
    let mut out = Vec::new();
    let now = crate::db::now_ms();
    let chains = crate::chain::list_chains()?;
    let chain = match chains.into_iter().find(|c| c.chain_id == chain_id) {
        Some(c) => c,
        None => {
            return Err(crate::error::AppError::NotFound(format!(
                "chain {chain_id}"
            )));
        }
    };
    let drop = eligibility::fetch_drop_or_none(&chain.rpc_url, &collection).await;
    let drop_ref = drop.as_ref();
    let expected_network = u64::try_from(chain.chain_id).unwrap_or(0);

    for id in wallet_ids {
        let w = match wallet_store::get_wallet(id) {
            Ok(w) => w,
            Err(e) => {
                out.push(EligibilityResult {
                    wallet_id: Some(id),
                    address: format!("wallet #{id} not found"),
                    collection: collection.clone(),
                    eligible: None,
                    detail: format!("error: {e}"),
                    checked_at: now,
                });
                continue;
            }
        };

        // OpenSea stages (FCFS/presale) first when vault unlocked; fall back to SeaDrop.
        let mut handled = false;
        if crate::vault::is_unlocked().unwrap_or(false) {
            match opensea::check_wallet(id, &w.address, &collection, expected_network).await {
                Ok(Some((eligible, detail))) => {
                    let _ = crate::db::with_conn(|conn| {
                        conn.execute(
                            "INSERT INTO eligibility_checks(wallet_id, collection, result, detail, checked_at)
                             VALUES (?1,?2,?3,?4,?5)",
                            rusqlite::params![id, collection, eligible as i64, detail, now],
                        )?;
                        Ok(())
                    });
                    out.push(EligibilityResult {
                        wallet_id: Some(id),
                        address: w.address.clone(),
                        collection: collection.clone(),
                        eligible: Some(eligible),
                        detail,
                        checked_at: now,
                    });
                    handled = true;
                }
                Ok(None) => {}
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("vault locked") {
                        // fall through to SeaDrop-only
                    } else if msg.contains("authentication required")
                        || msg.contains("session wallet mismatch")
                    {
                        wallet_store::log_activity(
                            "eligibility.opensea",
                            &format!("OpenSea auth failed for #{id}: {msg}"),
                            None,
                            false,
                        );
                    } else {
                        wallet_store::log_activity(
                            "eligibility.opensea",
                            &format!("OpenSea check failed for #{id}: {msg}"),
                            None,
                            false,
                        );
                    }
                }
            }
        }
        if handled {
            continue;
        }

        match eligibility::check_eligibility(&chain.rpc_url, &w.address, &collection, drop_ref)
            .await
        {
            Ok((eligible, detail)) => {
                crate::db::with_conn(|conn| {
                    conn.execute(
                        "INSERT INTO eligibility_checks(wallet_id, collection, result, detail, checked_at)
                         VALUES (?1,?2,?3,?4,?5)",
                        rusqlite::params![id, collection, eligible as i64, detail, now],
                    )?;
                    Ok(())
                })?;
                out.push(EligibilityResult {
                    wallet_id: Some(id),
                    address: w.address,
                    collection: collection.clone(),
                    eligible: Some(eligible),
                    detail,
                    checked_at: now,
                });
            }
            Err(e) => {
                wallet_store::log_activity(
                    "eligibility.error",
                    &format!("Eligibility check failed for #{id}: {e}"),
                    None,
                    false,
                );
                out.push(EligibilityResult {
                    wallet_id: Some(id),
                    address: w.address,
                    collection: collection.clone(),
                    // None = check failed (not the same as not-eligible)
                    eligible: None,
                    detail: format!("error: {e}"),
                    checked_at: now,
                });
            }
        }
    }
    let ok_count = out.iter().filter(|r| r.eligible == Some(true)).count();
    let via = out
        .iter()
        .filter(|r| r.detail.contains("via OpenSea"))
        .count();
    wallet_store::log_activity(
        "eligibility",
        &format!(
            "Eligibility check: {ok_count}/{} eligible for {collection} ({via} via OpenSea)",
            out.len()
        ),
        None,
        true,
    );
    Ok(out)
}

#[derive(Serialize)]
pub struct EligibilityHistoryRow {
    pub id: i64,
    pub wallet_id: Option<i64>,
    pub collection: String,
    pub result: i64,
    pub detail: Option<String>,
    pub checked_at: i64,
}

// ─── Stage matrix ────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StageMatrixCell {
    pub stage_name: String,
    pub eligible: bool,
    pub max_quantity: Option<u64>,
    pub price_usd: Option<f64>,
    /// Stage open time (unix ms) when OpenSea reports it.
    pub starts_at_ms: Option<i64>,
    /// "live" | "not_started" | "ended" | "unknown" — derived from start/end times.
    pub live_status: String,
    /// Human hint: "Live now" / "Opens Sep 28 12:00 UTC" / "—".
    pub schedule_hint: String,
    /// True only when eligible AND live (or schedule unknown but eligible).
    /// The enqueue guard refuses anything that is not actionable.
    pub actionable: bool,
}

#[derive(Serialize)]
pub struct StageMatrixRow {
    pub wallet_id: Option<i64>,
    pub address: String,
    pub collection: String,
    pub slug: String,
    pub stages: Vec<StageMatrixCell>,
    pub error: Option<String>,
    pub checked_at: i64,
}

#[derive(Serialize)]
pub struct StageMatrixResult {
    pub slug: String,
    pub collection: String,
    /// Union of all stage names seen across wallets, in order of first appearance.
    pub stage_names: Vec<String>,
    pub rows: Vec<StageMatrixRow>,
    pub checked_at: i64,
}

/// Hybrid eligibility matrix: OpenSea stages per wallet, SeaDrop fallback.
/// Returns a matrix keyed by collection slug for the table view.
#[tauri::command]
pub async fn eligibility_matrix_run(
    wallet_ids: Vec<i64>,
    chain_id: i64,
    collection: String,
) -> AppResult<StageMatrixResult> {
    let now = crate::db::now_ms();
    let chains = crate::chain::list_chains()?;
    let chain = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| crate::error::AppError::NotFound(format!("chain {chain_id}")))?;

    let expected_network = u64::try_from(chain.chain_id).unwrap_or(0);
    let drop = crate::chain::eligibility::fetch_drop_or_none(&chain.rpc_url, &collection).await;
    let drop_ref = drop.as_ref();
    let vault_ok = crate::vault::is_unlocked().unwrap_or(false);

    // Resolve slug once (OpenSea) — fall back to the collection address.
    let mut slug = collection.clone();
    let mut stage_names: Vec<String> = Vec::new();
    let mut rows: Vec<StageMatrixRow> = Vec::new();

    for id in &wallet_ids {
        let w = match wallet_store::get_wallet(*id) {
            Ok(w) => w,
            Err(e) => {
                rows.push(StageMatrixRow {
                    wallet_id: Some(*id),
                    address: format!("wallet #{id} not found"),
                    collection: collection.clone(),
                    slug: slug.clone(),
                    stages: Vec::new(),
                    error: Some(format!("{e}")),
                    checked_at: now,
                });
                continue;
            }
        };

        // ── Path A: OpenSea per-stage check ──────────────────────────
        let mut stage_cells: Option<Vec<StageMatrixCell>> = None;
        let mut err_msg: Option<String> = None;
        if vault_ok {
            match opensea::check_wallet_stages(*id, &w.address, &collection, expected_network)
                .await
            {
                Ok(Some((os_slug, results))) => {
                    slug = os_slug;
                    let cells: Vec<StageMatrixCell> = results
                        .iter()
                        .map(|r| {
                            // Actionable = eligible AND the stage is open (live) or
                            // the schedule is unknown (can't prove it's closed →
                            // allow). Stages that already opened and closed, or
                            // that have not opened yet, are never actionable.
                            let actionable = r.eligible
                                && matches!(r.live_status.as_str(), "live" | "unknown");
                            StageMatrixCell {
                                stage_name: r.stage_type.clone(),
                                eligible: r.eligible,
                                max_quantity: r.max_quantity,
                                price_usd: r.price_usd,
                                starts_at_ms: r.starts_at_ms,
                                live_status: r.live_status.clone(),
                                schedule_hint: r.schedule_hint.clone(),
                                actionable,
                            }
                        })
                        .collect();
                    for c in &cells {
                        if !stage_names.contains(&c.stage_name) {
                            stage_names.push(c.stage_name.clone());
                        }
                    }
                    stage_cells = Some(cells);
                }
                Ok(None) => {} // not on OpenSea → fall through to SeaDrop
                Err(e) => {
                    let msg = e.to_string();
                    if !msg.contains("vault locked") {
                        err_msg = Some(msg);
                    }
                }
            }
        }

        // ── Path B: SeaDrop / on-chain fallback ─────────────────────
        if stage_cells.is_none() {
            match crate::chain::eligibility::check_eligibility(
                &chain.rpc_url,
                &w.address,
                &collection,
                drop_ref,
            )
            .await
            {
                Ok((eligible, detail)) => {
                    let cell = StageMatrixCell {
                        stage_name: "ONCHAIN".into(),
                        eligible,
                        max_quantity: None,
                        price_usd: None,
                        starts_at_ms: None,
                        live_status: "unknown".into(),
                        schedule_hint: "—".into(),
                        actionable: eligible,
                    };
                    if !stage_names.contains(&cell.stage_name) {
                        stage_names.push(cell.stage_name.clone());
                    }
                    stage_cells = Some(vec![cell]);
                    // Persist to legacy table for backward compat.
                    let _ = crate::db::with_conn(|conn| {
                        conn.execute(
                            "INSERT INTO eligibility_checks(wallet_id, collection, result, detail, checked_at)
                             VALUES (?1,?2,?3,?4,?5)",
                            rusqlite::params![id, collection, eligible as i64, detail, now],
                        )?;
                        Ok(())
                    });
                }
                Err(e) => {
                    err_msg = Some(format!("onchain: {e}"));
                }
            }
        }

        let stages = stage_cells.unwrap_or_default();

        // Persist matrix cells.
        let _ = crate::db::with_conn(|conn| {
            for s in &stages {
                conn.execute(
                    "INSERT INTO eligibility_matrix(wallet_id, collection, stage_name, eligible, max_quantity, detail, checked_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    rusqlite::params![
                        id,
                        collection,
                        s.stage_name,
                        s.eligible as i64,
                        s.max_quantity,
                        err_msg,
                        now
                    ],
                )?;
            }
            Ok(())
        });

        rows.push(StageMatrixRow {
            wallet_id: Some(*id),
            address: w.address.clone(),
            collection: collection.clone(),
            slug: slug.clone(),
            stages,
            error: err_msg,
            checked_at: now,
        });
    }

    let total_wallets = rows.len();
    let any_eligible = rows.iter().filter(|r| r.stages.iter().any(|s| s.eligible)).count();
    wallet_store::log_activity(
        "eligibility.matrix",
        &format!(
            "Matrix check: {any_eligible}/{total_wallets} eligible for {slug} (stages: {})",
            stage_names.join(", ")
        ),
        None,
        true,
    );

    Ok(StageMatrixResult {
        slug,
        collection,
        stage_names,
        rows,
        checked_at: now,
    })
}

#[tauri::command]
pub fn eligibility_history(limit: Option<u32>) -> AppResult<Vec<EligibilityHistoryRow>> {
    let lim = limit.unwrap_or(50);
    crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, wallet_id, collection, result, detail, checked_at
             FROM eligibility_checks ORDER BY checked_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([lim], |r| {
            Ok(EligibilityHistoryRow {
                id: r.get(0)?,
                wallet_id: r.get(1)?,
                collection: r.get(2)?,
                result: r.get(3)?,
                detail: r.get(4)?,
                checked_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}
