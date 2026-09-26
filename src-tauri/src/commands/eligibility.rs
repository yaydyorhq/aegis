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
