use crate::error::{AppError, AppResult};
use crate::vault;
use alloy::primitives::U256;
use serde::Serialize;
use std::time::Duration;

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

/// One wallet's live native balance on one chain.
#[derive(Serialize)]
pub struct LiveWalletBalance {
    pub wallet_id: i64,
    /// ETH decimal string (wei math done backend-side); None when the RPC
    /// failed for this wallet — excluded from the chain total, counted in
    /// `failed`. Label/address resolve client-side from the wallet store.
    pub balance_eth: Option<String>,
}

#[derive(Serialize)]
pub struct LiveChainBalance {
    pub chain_id: i64,
    pub chain_name: String,
    pub native_symbol: String,
    /// Sum over wallets that answered, ETH decimal string.
    pub total_eth: String,
    pub failed: u32,
    /// One entry per vault wallet, richest first.
    pub balances: Vec<LiveWalletBalance>,
}

#[derive(Serialize)]
pub struct PortfolioLive {
    /// Every enabled chain, same order as chain_list.
    pub chains: Vec<LiveChainBalance>,
    pub fetched_at: i64,
}

/// Live native balances for every wallet across EVERY enabled chain, meant
/// for a ~20s UI poll. One bounded task pool (8 in flight) and a 6s cap per
/// call, so a dead provider slows one snapshot instead of hanging it — its
/// wallets come back as `failed` and the other chains stay healthy.
#[tauri::command]
pub async fn portfolio_live() -> AppResult<PortfolioLive> {
    const CONCURRENCY: usize = 8;
    const PER_CALL_TIMEOUT: Duration = Duration::from_secs(6);

    let chains = crate::chain::list_chains()?;
    let wallets = crate::wallet_store::list_wallets()?;
    let enabled: Vec<&crate::chain::ChainRow> =
        chains.iter().filter(|c| c.enabled != 0).collect();

    // Work list in chain-major order: the first chain fills first even
    // though the pool drains globally.
    let mut queue: Vec<(usize, usize)> = Vec::new();
    for ci in 0..enabled.len() {
        for wi in 0..wallets.len() {
            queue.push((ci, wi));
        }
    }

    let mut set = tokio::task::JoinSet::new();
    let spawn_one = |set: &mut tokio::task::JoinSet<(usize, usize, Option<U256>)>,
                     chain: &crate::chain::ChainRow,
                     ci: usize,
                     wi: usize,
                     address: &str| {
        let rpc = chain.rpc_url.clone();
        let address = address.to_string();
        set.spawn(async move {
            let bal = match tokio::time::timeout(
                PER_CALL_TIMEOUT,
                crate::chain::native_balance(&rpc, &address),
            )
            .await
            {
                Ok(Ok(hex_bal)) => {
                    U256::from_str_radix(hex_bal.trim_start_matches("0x"), 16).ok()
                }
                _ => None,
            };
            (ci, wi, bal)
        });
    };

    let mut next = 0usize;
    let mut grid: Vec<Vec<Option<U256>>> = vec![vec![None; wallets.len()]; enabled.len()];
    while set.len() < CONCURRENCY && next < queue.len() {
        let (ci, wi) = queue[next];
        spawn_one(&mut set, enabled[ci], ci, wi, &wallets[wi].address);
        next += 1;
    }
    while let Some(res) = set.join_next().await {
        let (ci, wi, bal) = res.map_err(|e| AppError::Other(e.to_string()))?;
        grid[ci][wi] = bal;
        if next < queue.len() {
            let (ci2, wi2) = queue[next];
            spawn_one(&mut set, enabled[ci2], ci2, wi2, &wallets[wi2].address);
            next += 1;
        }
    }

    let mut out = Vec::with_capacity(enabled.len());
    for (ci, chain) in enabled.iter().enumerate() {
        let mut total = U256::ZERO;
        let mut failed = 0u32;
        let mut rows = Vec::with_capacity(wallets.len());
        for (wi, w) in wallets.iter().enumerate() {
            match grid[ci][wi] {
                Some(v) => {
                    total = total.saturating_add(v);
                    rows.push(LiveWalletBalance {
                        wallet_id: w.id,
                        balance_eth: Some(crate::wallet::balance_to_eth(v)),
                    });
                }
                None => {
                    failed += 1;
                    rows.push(LiveWalletBalance {
                        wallet_id: w.id,
                        balance_eth: None,
                    });
                }
            }
        }
        rows.sort_by(|a, b| {
            let av = a.balance_eth.as_deref().and_then(|s| s.parse::<f64>().ok());
            let bv = b.balance_eth.as_deref().and_then(|s| s.parse::<f64>().ok());
            bv.partial_cmp(&av).unwrap_or(std::cmp::Ordering::Equal)
        });
        out.push(LiveChainBalance {
            chain_id: chain.chain_id,
            chain_name: chain.name.clone(),
            native_symbol: chain.symbol.clone(),
            total_eth: crate::wallet::balance_to_eth(total),
            failed,
            balances: rows,
        });
    }

    Ok(PortfolioLive {
        chains: out,
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
