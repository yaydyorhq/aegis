use crate::error::{AppError, AppResult};
use crate::vault;
use alloy::primitives::U256;
use serde::Serialize;

#[derive(Serialize)]
pub struct PortfolioCollectionSummary {
    pub contract: String,
    pub chain_id: i64,
    pub native_symbol: String,
    pub net_eth: Option<String>,
    pub roi_pct: Option<f64>,
    pub spent_eth: String,
    pub gas_eth: String,
    pub holding: u64,
    pub balance: u64,
    pub floor_eth: Option<String>,
    pub floor_source: Option<String>,
    pub scanned_at: i64,
    pub wallets: u64,
}

#[derive(Serialize)]
pub struct PortfolioStats {
    /// Distinct wallets that have at least one portfolio scan.
    pub wallets: i64,
    /// Number of (wallet, chain) latest-scan pairs summed.
    pub pairs: i64,
    /// Summed `net_native_flow` across latest scans; None when no scan reports it.
    pub net_flow_eth: Option<String>,
    pub latest_at: Option<i64>,
    /// Latest persisted Collection PnL scan (Dashboard headline data).
    pub collection: Option<PortfolioCollectionSummary>,
}

#[derive(Serialize)]
pub struct StatsOverview {
    pub wallets: i64,
    pub tasks: i64,
    pub rpc_endpoints: i64,
    pub eligible_checks: i64,
    pub activity_records: i64,
    pub vault_unlocked: bool,
    pub api_providers: i64,
    pub portfolio: PortfolioStats,
}

#[derive(Serialize)]
pub struct ModuleStatusItem {
    pub label: String,
    pub status: String,
    pub detail: String,
    pub ok: bool,
}

/// One wallet's live native balance.
#[derive(Serialize)]
pub struct LiveWalletBalance {
    pub wallet_id: i64,
    pub label: String,
    pub address: String,
    /// ETH decimal string (wei math done backend-side); None when the RPC
    /// failed for this wallet — excluded from the total, surfaced via `failed`.
    pub balance_eth: Option<String>,
}

#[derive(Serialize)]
pub struct PortfolioLive {
    pub chain_id: i64,
    pub chain_name: String,
    pub native_symbol: String,
    /// Sum over wallets that answered, ETH decimal string.
    pub total_eth: String,
    pub wallets: Vec<LiveWalletBalance>,
    pub failed: u32,
    pub fetched_at: i64,
}

/// Live native balances for every wallet, meant for a ~20s UI poll.
/// Chain anchor: the newest Collection PnL scan's chain, else the first
/// enabled chain. Bounded concurrency so a 500-wallet vault cannot flood a
/// public RPC from one tick.
#[tauri::command]
pub async fn portfolio_live() -> AppResult<PortfolioLive> {
    const CONCURRENCY: usize = 6;
    let chains = crate::chain::list_chains()?;
    let scanned_chain: Option<i64> = crate::db::with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT chain_id FROM collection_pnl_scans ORDER BY scanned_at DESC LIMIT 1",
                [],
                |r| r.get(0),
            )
            .ok())
    })?;
    let anchor = scanned_chain.unwrap_or_else(|| {
        chains
            .iter()
            .find(|c| c.enabled != 0)
            .map(|c| c.chain_id)
            .unwrap_or(0)
    });
    let chain = chains
        .into_iter()
        .find(|c| c.chain_id == anchor && c.enabled != 0)
        .ok_or_else(|| AppError::NotFound("no enabled chain for the live scan".into()))?;

    let wallets = crate::wallet_store::list_wallets()?;
    let rpc = chain.rpc_url.clone();
    let mut queue = wallets.iter().enumerate();
    let mut set = tokio::task::JoinSet::new();
    let mut balances: Vec<Option<U256>> = vec![None; wallets.len()];

    let spawn_one = |set: &mut tokio::task::JoinSet<(usize, Option<U256>)>,
                     rpc: &str,
                     i: usize,
                     address: &str| {
        let rpc = rpc.to_string();
        let address = address.to_string();
        set.spawn(async move {
            let bal = crate::chain::native_balance(&rpc, &address)
                .await
                .ok()
                .and_then(|h| U256::from_str_radix(h.trim_start_matches("0x"), 16).ok());
            (i, bal)
        });
    };
    while set.len() < CONCURRENCY {
        match queue.next() {
            Some((i, w)) => spawn_one(&mut set, &rpc, i, &w.address),
            None => break,
        }
    }
    while let Some(res) = set.join_next().await {
        let (i, bal) = res.map_err(|e| AppError::Other(e.to_string()))?;
        balances[i] = bal;
        if let Some((i, w)) = queue.next() {
            spawn_one(&mut set, &rpc, i, &w.address);
        }
    }

    let mut total = U256::ZERO;
    let mut rows = Vec::with_capacity(wallets.len());
    let mut failed = 0u32;
    for (w, bal) in wallets.iter().zip(balances.iter()) {
        let balance_eth = bal.map(|v| {
            total = total.saturating_add(v);
            crate::wallet::balance_to_eth(v)
        });
        if bal.is_none() {
            failed += 1;
        }
        rows.push(LiveWalletBalance {
            wallet_id: w.id,
            label: w.label.clone(),
            address: w.address.clone(),
            balance_eth,
        });
    }
    rows.sort_by(|a, b| {
        let av = a.balance_eth.as_deref().and_then(|s| s.parse::<f64>().ok());
        let bv = b.balance_eth.as_deref().and_then(|s| s.parse::<f64>().ok());
        bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(PortfolioLive {
        chain_id: chain.chain_id,
        chain_name: chain.name.clone(),
        native_symbol: chain.symbol.clone(),
        total_eth: crate::wallet::balance_to_eth(total),
        wallets: rows,
        failed,
        fetched_at: crate::db::now_ms(),
    })
}

/// Parse a signed ETH flow string ("+1.5", "-0.25", "0") to f64.
pub(crate) fn parse_flow(v: &str) -> Option<f64> {
    let t = v.trim().trim_start_matches('+');
    if t.is_empty() {
        return None;
    }
    t.parse::<f64>().ok().filter(|f| f.is_finite())
}

/// Sum `net_native_flow` across pnl_scans payload JSONs.
/// Returns (total, count_with_value). Legacy payloads without the field are skipped.
pub(crate) fn sum_net_flows(payloads: &[String]) -> (f64, i64) {
    let mut total = 0.0f64;
    let mut n = 0i64;
    for p in payloads {
        let v: serde_json::Value = match serde_json::from_str(p) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(s) = v.get("net_native_flow").and_then(|x| x.as_str()) {
            if let Some(f) = parse_flow(s) {
                total += f;
                n += 1;
            }
        }
    }
    (total, n)
}

fn fmt_eth(v: f64) -> String {
    let s = format!("{:.6}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

fn summarize_collection(r: &crate::collection_pnl::CollectionPnlResult) -> PortfolioCollectionSummary {
    PortfolioCollectionSummary {
        contract: r.contract.clone(),
        chain_id: r.chain_id,
        native_symbol: r.native_symbol.clone(),
        net_eth: r.totals.net_eth.clone(),
        roi_pct: r.totals.roi_pct,
        spent_eth: r.totals.spent_eth.clone(),
        gas_eth: r.totals.gas_eth.clone(),
        holding: r.totals.holding,
        balance: r.totals.balance,
        floor_eth: r.floor_eth.clone(),
        floor_source: r.floor_source.clone(),
        scanned_at: r.scanned_at,
        wallets: r.totals.wallets,
    }
}

#[tauri::command]
pub fn stats_overview() -> AppResult<StatsOverview> {
    // DB work first (with_conn holds a global mutex — no nested calls inside).
    let (base, payloads, latest_at) = crate::db::with_conn(|conn| {
        let count = |sql: &str| -> AppResult<i64> {
            let mut stmt = conn.prepare(sql)?;
            let n: i64 = stmt.query_row([], |r| r.get(0))?;
            Ok(n)
        };

        let wallets_total = count("SELECT COUNT(DISTINCT wallet_id) FROM pnl_scans")?;
        let latest_at: Option<i64> = {
            let mut stmt = conn.prepare("SELECT MAX(scanned_at) FROM pnl_scans")?;
            stmt.query_row([], |r| r.get(0))?
        };
        // Latest scan per (wallet, chain) — one payload per pair.
        let mut payloads: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare(
                "SELECT p.payload FROM pnl_scans p
                 JOIN (SELECT wallet_id, chain_id, MAX(scanned_at) AS m
                       FROM pnl_scans GROUP BY wallet_id, chain_id) x
                   ON p.wallet_id = x.wallet_id AND p.chain_id = x.chain_id
                  AND p.scanned_at = x.m",
            )?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
            for row in rows {
                payloads.push(row?);
            }
        }

        Ok((
            (
                count("SELECT COUNT(*) FROM wallets")?,
                count(
                    "SELECT COUNT(*) FROM mint_tasks WHERE status IN ('pending','signing','broadcasting')",
                )?,
                count("SELECT COUNT(*) FROM chains WHERE enabled = 1")?,
                count("SELECT COUNT(*) FROM eligibility_checks")?,
                count("SELECT COUNT(*) FROM activity")?,
                vault::is_unlocked().unwrap_or(false),
                count("SELECT COUNT(*) FROM api_keys")?,
                wallets_total,
            ),
            payloads,
            latest_at,
        ))
    })?;

    let pairs = payloads.len() as i64;
    let (total, n) = sum_net_flows(&payloads);
    let net_flow_eth = (n > 0).then(|| fmt_eth(total));

    let collection = crate::commands::pnl::collection_pnl_last()?
        .as_ref()
        .map(summarize_collection);

    Ok(StatsOverview {
        wallets: base.0,
        tasks: base.1,
        rpc_endpoints: base.2,
        eligible_checks: base.3,
        activity_records: base.4,
        vault_unlocked: base.5,
        api_providers: base.6,
        portfolio: PortfolioStats {
            wallets: base.7,
            pairs,
            net_flow_eth,
            latest_at,
            collection,
        },
    })
}

#[tauri::command]
pub fn module_status() -> AppResult<Vec<ModuleStatusItem>> {
    let s = stats_overview()?;
    Ok(vec![
        ModuleStatusItem {
            label: "Activity ledger".into(),
            status: "ok".into(),
            detail: format!("{} records", s.activity_records),
            ok: true,
        },
        ModuleStatusItem {
            label: "Local vault".into(),
            status: if s.vault_unlocked { "unlocked" } else { "locked" }.into(),
            detail: if s.vault_unlocked { "Unlocked" } else { "Locked" }.into(),
            ok: s.vault_unlocked,
        },
        ModuleStatusItem {
            label: "RPC network".into(),
            status: if s.rpc_endpoints > 0 { "configured" } else { "none" }.into(),
            detail: if s.rpc_endpoints > 0 {
                "Configured".into()
            } else {
                "No endpoints".into()
            },
            ok: s.rpc_endpoints > 0,
        },
        ModuleStatusItem {
            label: "API providers".into(),
            status: if s.api_providers > 0 { "configured" } else { "none" }.into(),
            detail: if s.api_providers > 0 {
                "Configured".into()
            } else {
                "None".into()
            },
            ok: s.api_providers > 0,
        },
        ModuleStatusItem {
            label: "Mint tasks".into(),
            status: "queue".into(),
            detail: s.tasks.to_string(),
            ok: true,
        },
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_flow_strings() {
        assert_eq!(parse_flow("+0.5"), Some(0.5));
        assert_eq!(parse_flow("-0.25"), Some(-0.25));
        assert_eq!(parse_flow("0"), Some(0.0));
        assert_eq!(parse_flow("+1"), Some(1.0));
        assert_eq!(parse_flow("+"), None);
        assert_eq!(parse_flow(""), None);
        assert_eq!(parse_flow("abc"), None);
        assert_eq!(parse_flow("n/a"), None);
    }

    #[test]
    fn sum_flows_skips_null_and_legacy() {
        let payloads = vec![
            r#"{"net_native_flow":"+1.5"}"#.to_string(),
            r#"{"net_native_flow":"-0.5"}"#.to_string(),
            r#"{"balance_wei":"0x99","chain_id":4663,"window_blocks":50000}"#.to_string(),
            r#"{"net_native_flow":null}"#.to_string(),
            r#"{"net_native_flow":"-0.25"}"#.to_string(),
            "not json".to_string(),
        ];
        let (total, n) = sum_net_flows(&payloads);
        assert_eq!(n, 3);
        assert!((total - 0.75).abs() < 1e-9);
        assert_eq!(fmt_eth(total), "0.75");
    }

    #[test]
    fn fmt_eth_trims_and_handles_negzero() {
        assert_eq!(fmt_eth(1.0), "1");
        assert_eq!(fmt_eth(-1.0), "-1");
        assert_eq!(fmt_eth(0.0), "0");
        assert_eq!(fmt_eth(-0.0000004), "0");
        assert_eq!(fmt_eth(0.123456789), "0.123457");
    }
}
