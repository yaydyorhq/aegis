use crate::chain;
use crate::error::{AppError, AppResult};
use crate::wallet_store;
use alloy::primitives::U256;
use serde::Serialize;
use std::collections::HashMap;

/// keccak("Transfer(address,address,uint256)") — shared by ERC-20 and ERC-721.
const TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";
const MAX_TOKENS: usize = 30;
const LOG_CHUNK: u64 = 2_000;

#[derive(Serialize, Clone)]
pub struct TokenPosition {
    pub contract: String,
    pub symbol: String,
    pub decimals: u8,
    /// Current balance, human units.
    pub balance: String,
    /// Net in-window flow (in − out), signed human units, "0" if none.
    pub net_flow: String,
}

#[derive(Serialize)]
pub struct PnlScanResult {
    pub wallet_id: i64,
    pub chain_id: i64,
    pub status: String,
    /// "explorer" (etherscan-style API) | "logs" (RPC log scan) | "native-only".
    pub source: String,
    pub native_symbol: String,
    pub native_balance: String,
    /// Net native flow over the window (explorer txlist only), else None.
    pub net_native_flow: Option<String>,
    pub tokens: Vec<TokenPosition>,
    /// Kept for UI compatibility: native balance as "unrealized".
    pub unrealized_eth: String,
    /// No cost-basis data available — always "0".
    pub realized_eth: String,
    pub note: Option<String>,
    pub scanned_at: i64,
    pub window_blocks: u64,
    pub from_block: u64,
    pub to_block: u64,
}

#[derive(Default, Clone)]
struct FlowAccum {
    inflow: U256,
    outflow: U256,
}

pub(crate) fn format_units(v: U256, decimals: u8) -> String {
    let d = u32::from(decimals.min(36));
    let base = U256::from(10u64).pow(U256::from(d));
    let whole = v / base;
    let frac = v % base;
    if frac.is_zero() {
        return whole.to_string();
    }
    let mut fs = format!("{:0width$}", frac, width = d as usize);
    while fs.ends_with('0') {
        fs.pop();
    }
    format!("{}.{}", whole, fs)
}

fn signed_flow(inflow: U256, outflow: U256) -> String {
    if inflow > outflow {
        format!("+{}", format_units(inflow - outflow, 18))
    } else if outflow > inflow {
        format!("-{}", format_units(outflow - inflow, 18))
    } else {
        "0".into()
    }
}

fn addr_topic(address: &str) -> String {
    let a = address.trim().trim_start_matches("0x").to_lowercase();
    format!("0x{:0>64}", a)
}

fn topic_to_addr(topic: &str) -> String {
    let t = topic.trim_start_matches("0x");
    if t.len() >= 40 {
        format!("0x{}", t[t.len() - 40..].to_lowercase())
    } else {
        format!("0x{t}")
    }
}

fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

async fn explorer_api(
    explorer: &str,
    query: &str,
) -> AppResult<serde_json::Value> {
    let base = explorer.trim_end_matches('/');
    let mut url = format!("{base}/api?{query}");
    // Etherscan-family explorers rate-limit unkeyed requests hard; attach the
    // stored key (API Settings, provider "etherscan") when one exists. Non-
    // etherscan explorers (Blockscout) ignore the extra param. Only URL-safe
    // key characters are kept so a pasted key cannot inject query params.
    if let Some(key) = crate::commands::api_keys::lookup("etherscan") {
        let safe: String = key.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if !safe.is_empty() {
            url.push_str(&format!("&apikey={safe}"));
        }
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .map_err(|e| AppError::Rpc(format!("http client: {e}")))?;
    let v: serde_json::Value = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AppError::Rpc(format!("explorer: {e}")))?
        .json()
        .await
        .map_err(|e| AppError::Rpc(format!("explorer json: {e}")))?;
    // etherscan-style error payload: result is a string message
    if v.get("status").and_then(|s| s.as_str()) == Some("0") {
        if let Some(msg) = v.get("message").and_then(|m| m.as_str()) {
            if v.get("result").map(|r| r.is_string()).unwrap_or(false) {
                return Err(AppError::Rpc(format!("explorer: {msg}")));
            }
        }
    }
    Ok(v)
}

/// Scan ERC-20 Transfer logs (3-topic form) in [from, to] for flows involving the wallet.
async fn scan_erc20_flows(
    rpc_url: &str,
    wallet: &str,
    from: u64,
    to: u64,
) -> AppResult<HashMap<String, FlowAccum>> {
    let wallet_topic = addr_topic(wallet);
    let wallet_lc = wallet.to_lowercase();
    let mut flows: HashMap<String, FlowAccum> = HashMap::new();
    if to < from {
        return Ok(flows);
    }
    let mut start = from;
    while start <= to {
        let end = (start + LOG_CHUNK - 1).min(to);
        // inbound: topic2 = wallet
        let logs_in = chain::get_logs(
            rpc_url,
            start,
            end,
            None,
            vec![
                Some(TRANSFER_TOPIC.to_string()),
                None,
                Some(wallet_topic.clone()),
            ],
        )
        .await?;
        if let Some(arr) = logs_in.as_array() {
            for (contract, value) in parse_erc20_flows(arr, &wallet_lc, false) {
                let e = flows.entry(contract).or_default();
                e.inflow = e.inflow.saturating_add(value);
            }
        }
        // outbound: topic1 = wallet
        let logs_out = chain::get_logs(
            rpc_url,
            start,
            end,
            None,
            vec![
                Some(TRANSFER_TOPIC.to_string()),
                Some(wallet_topic.clone()),
                None,
            ],
        )
        .await?;
        if let Some(arr) = logs_out.as_array() {
            for (contract, value) in parse_erc20_flows(arr, &wallet_lc, true) {
                let e = flows.entry(contract).or_default();
                e.outflow = e.outflow.saturating_add(value);
            }
        }
        start = end + 1;
    }
    Ok(flows)
}

/// Extract (contract, value) for 3-topic (ERC-20) Transfer logs only.
fn parse_erc20_flows(
    logs: &[serde_json::Value],
    wallet: &str,
    outbound: bool,
) -> Vec<(String, U256)> {
    let mut out = Vec::new();
    for log in logs {
        let topics = match log.get("topics").and_then(|t| t.as_array()) {
            Some(t) if t.len() == 3 => t,
            _ => continue,
        };
        let contract = log
            .get("address")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_lowercase();
        if !is_address(&contract) {
            continue;
        }
        // verify the wallet side matches the RPC filter direction:
        // inbound query filters topic2 (to), outbound query filters topic1 (from)
        let wallet_side = topic_to_addr(topics[if outbound { 1 } else { 2 }].as_str().unwrap_or(""));
        if wallet_side.to_lowercase() != wallet {
            continue;
        }
        let raw = log.get("data").and_then(|d| d.as_str()).unwrap_or("0x");
        let h = raw.trim_start_matches("0x");
        let value = U256::from_str_radix(h, 16).unwrap_or(U256::ZERO);
        out.push((contract, value));
    }
    out
}

/// Fetch up to `max_pages`×1000 records of an etherscan-style account action.
/// The old single call silently truncated a busy wallet's window at 1000 —
/// flows were understated without any warning. A failing page keeps the
/// partial data (better than none); `max_pages` caps total requests.
async fn explorer_paged(
    explorer: &str,
    base_query: &str,
    max_pages: u32,
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for page in 1..=max_pages {
        let query = format!("{base_query}&page={page}&offset=1000&sort=asc");
        let v = match explorer_api(explorer, &query).await {
            Ok(v) => v,
            Err(_) => break,
        };
        match v.get("result").and_then(|r| r.as_array()) {
            Some(arr) => {
                let n = arr.len();
                out.extend(arr.iter().cloned());
                if n < 1000 {
                    break;
                }
            }
            None => break,
        }
    }
    out
}

/// Fallback portfolio discovery: unique contracts from transfer logs → balance/symbol/decimals.
async fn tokens_from_logs(
    rpc_url: &str,
    wallet: &str,
    flows: &HashMap<String, FlowAccum>,
) -> Vec<TokenPosition> {
    let mut ordered: Vec<(&String, &FlowAccum)> = flows.iter().collect();
    // most active contracts first, cap to bound RPC cost
    ordered.sort_by(|a, b| {
        let act = |f: &FlowAccum| f.inflow + f.outflow;
        act(b.1).cmp(&act(a.1))
    });
    ordered.truncate(MAX_TOKENS);
    let mut out = Vec::new();
    for (contract, flow) in ordered {
        let balance = chain::erc20::balance_of(rpc_url, contract, wallet)
            .await
            .unwrap_or(U256::ZERO);
        if balance.is_zero() && flow.inflow.is_zero() && flow.outflow.is_zero() {
            continue;
        }
        let decimals = chain::erc20::decimals(rpc_url, contract).await.unwrap_or(18);
        let symbol = chain::erc20::symbol(rpc_url, contract)
            .await
            .unwrap_or_else(|_| short_contract(contract));
        out.push(TokenPosition {
            contract: contract.clone(),
            symbol,
            decimals,
            balance: format_units(balance, decimals),
            net_flow: signed_units(flow.inflow, flow.outflow, decimals),
        });
    }
    out
}

fn signed_units(inflow: U256, outflow: U256, decimals: u8) -> String {
    if inflow > outflow {
        format!("+{}", format_units(inflow - outflow, decimals))
    } else if outflow > inflow {
        format!("-{}", format_units(outflow - inflow, decimals))
    } else {
        "0".into()
    }
}

fn short_contract(c: &str) -> String {
    if c.len() > 12 {
        format!("{}…{}", &c[0..7], &c[c.len() - 4..])
    } else {
        c.to_string()
    }
}

/// Portfolio + net-flow scan.
/// 1. Native balance via RPC (always).
/// 2. ERC-20 balances via explorer `tokenlist` when available, else via Transfer logs + balanceOf.
/// 3. Net flows via explorer `tokentx`/`txlist` (windowed) when available, else via logs.
pub async fn scan_pnl(wallet_id: i64, chain_id: i64, window_blocks: u64) -> AppResult<PnlScanResult> {
    let w = wallet_store::get_wallet(wallet_id)?;
    let chains = chain::list_chains()?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;

    let balance_hex = chain::native_balance(&chain_row.rpc_url, &w.address).await?;
    let balance_wei = u128::from_str_radix(balance_hex.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad balance".into()))?;
    let native_balance = crate::wallet::balance_to_eth(U256::from(balance_wei));

    let latest = chain::block_number(&chain_row.rpc_url).await?;
    let from_block = latest.saturating_sub(window_blocks);

    let mut source = "native-only".to_string();
    let mut status = "estimated".to_string();
    let mut tokens: Vec<TokenPosition> = Vec::new();
    let mut net_native_flow: Option<String> = None;
    let note: Option<String>;

    let mut flows: HashMap<String, FlowAccum> = HashMap::new();
    let explorer = chain_row.explorer.clone().filter(|e| !e.trim().is_empty());

    // ── Explorer path: tokenlist (balances) + tokentx/txlist (windowed flows) ──
    let mut explorer_ok = false;
    if let Some(ex) = explorer.as_deref() {
        let addr = w.address.to_lowercase();
        let tokenlist = explorer_api(
            ex,
            &format!(
                "module=account&action=tokenlist&address={addr}"
            ),
        )
        .await
        .ok();
        if let Some(v) = tokenlist {
            if let Some(arr) = v.get("result").and_then(|r| r.as_array()) {
                for t in arr {
                    let contract = t
                        .get("contractAddress")
                        .or_else(|| t.get("contract_address"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("");
                    if !is_address(contract) {
                        continue;
                    }
                    let symbol = t
                        .get("symbol")
                        .or_else(|| t.get("tokenSymbol"))
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    let decimals = t
                        .get("decimals")
                        .or_else(|| t.get("tokenDecimal"))
                        .and_then(|x| x.as_str().and_then(|s| s.parse::<u8>().ok()))
                        .or_else(|| t.get("decimals").and_then(|x| x.as_u64()).and_then(|n| u8::try_from(n).ok()))
                        .unwrap_or(18);
                    let balance_raw = t
                        .get("balance")
                        .and_then(|x| x.as_str())
                        .and_then(|s| s.parse::<U256>().ok())
                        .unwrap_or(U256::ZERO);
                    if balance_raw.is_zero() {
                        continue;
                    }
                    tokens.push(TokenPosition {
                        contract: contract.to_lowercase(),
                        symbol: if symbol.is_empty() {
                            short_contract(contract)
                        } else {
                            symbol
                        },
                        decimals,
                        balance: format_units(balance_raw, decimals),
                        net_flow: "0".into(),
                    });
                }
                explorer_ok = !tokens.is_empty();
            }
        }

        // ERC-20 windowed flows from tokentx (paged — up to 10×1000 records)
        let tokentx = explorer_paged(
            ex,
            &format!(
                "module=account&action=tokentx&address={addr}&startblock={from_block}&endblock={latest}"
            ),
            10,
        )
        .await;
        if !tokentx.is_empty() {
            for t in &tokentx {
                if t.get("isError").and_then(|x| x.as_str()) == Some("1") {
                    continue;
                }
                let contract = t
                    .get("contractAddress")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_lowercase();
                if !is_address(&contract) {
                    continue;
                }
                let from = t.get("from").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
                let to = t.get("to").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
                let value = t
                    .get("value")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<U256>().ok())
                    .unwrap_or(U256::ZERO);
                if from == addr {
                    let e = flows.entry(contract.clone()).or_default();
                    e.outflow = e.outflow.saturating_add(value);
                }
                if to == addr {
                    let e = flows.entry(contract).or_default();
                    e.inflow = e.inflow.saturating_add(value);
                }
            }
            explorer_ok = true;
        }

        // Native windowed flows from txlist (paged — up to 10×1000 records)
        let txlist = explorer_paged(
            ex,
            &format!(
                "module=account&action=txlist&address={addr}&startblock={from_block}&endblock={latest}"
            ),
            10,
        )
        .await;
        if !txlist.is_empty() {
            let mut inflow = U256::ZERO;
            let mut outflow = U256::ZERO;
            for t in &txlist {
                if t.get("isError").and_then(|x| x.as_str()) == Some("1") {
                    continue;
                }
                let from = t.get("from").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
                let to = t.get("to").and_then(|x| x.as_str()).unwrap_or("").to_lowercase();
                let value = t
                    .get("value")
                    .and_then(|x| x.as_str())
                    .and_then(|s| s.parse::<U256>().ok())
                    .unwrap_or(U256::ZERO);
                if to == addr {
                    inflow = inflow.saturating_add(value);
                }
                if from == addr {
                    outflow = outflow.saturating_add(value);
                }
            }
            net_native_flow = Some(signed_flow(inflow, outflow));
        }
    }

    // ── RPC fallback: discover tokens + flows from Transfer logs ──
    if !explorer_ok {
        flows = scan_erc20_flows(&chain_row.rpc_url, &w.address, from_block, latest).await?;
        if !flows.is_empty() {
            tokens = tokens_from_logs(&chain_row.rpc_url, &w.address, &flows).await;
            source = "logs".into();
        }
    } else {
        source = "explorer".into();
    }

    // attach windowed net flows onto the balance rows (explorer path)
    if !flows.is_empty() {
        for t in tokens.iter_mut() {
            if let Some(f) = flows.get(&t.contract) {
                t.net_flow = signed_units(f.inflow, f.outflow, t.decimals);
            }
        }
        // tokens seen in flow window but missing from tokenlist (e.g. fully spent)
        for (contract, f) in flows.iter() {
            if tokens.iter().any(|t| &t.contract == contract) {
                continue;
            }
            let decimals = chain::erc20::decimals(&chain_row.rpc_url, contract)
                .await
                .unwrap_or(18);
            let symbol = chain::erc20::symbol(&chain_row.rpc_url, contract)
                .await
                .unwrap_or_else(|_| short_contract(contract));
            tokens.push(TokenPosition {
                contract: contract.clone(),
                symbol,
                decimals,
                balance: "0".into(),
                net_flow: signed_units(f.inflow, f.outflow, decimals),
            });
        }
        if tokens.len() > MAX_TOKENS {
            tokens.truncate(MAX_TOKENS);
        }
    }

    if source == "native-only" {
        note = Some(
            "No explorer API and no ERC-20 transfers in window — native balance only.".into(),
        );
    } else if source == "logs" {
        note = Some(
            "RPC log scan: ERC-20 portfolio/flows are heuristic over the block window; \
             native net flow unavailable without an explorer indexer."
                .into(),
        );
    } else {
        note = Some(
            "Explorer-based: balances are current, flows cover the selected window only. \
             Realized PnL needs cost basis (not tracked)."
                .into(),
        );
    }
    if source != "native-only" {
        status = "live".into();
    }

    let now = crate::db::now_ms();
    let result = PnlScanResult {
        wallet_id,
        chain_id,
        status,
        source,
        native_symbol: chain_row.symbol.clone(),
        native_balance: native_balance.clone(),
        net_native_flow,
        tokens,
        unrealized_eth: native_balance,
        realized_eth: "0".into(),
        note,
        scanned_at: now,
        window_blocks,
        from_block,
        to_block: latest,
    };

    let payload = serde_json::json!({
        "source": result.source,
        "native_symbol": result.native_symbol,
        "native_balance": result.native_balance,
        "net_native_flow": result.net_native_flow,
        "token_count": result.tokens.len(),
        "window_blocks": result.window_blocks,
        "from_block": result.from_block,
        "to_block": result.to_block,
    });

    crate::db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO pnl_scans(wallet_id, chain_id, payload, status, scanned_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![wallet_id, chain_id, payload.to_string(), result.status, now],
        )?;
        Ok(())
    })?;

    wallet_store::log_activity(
        "pnl.scan",
        &format!(
            "PnL scan wallet #{wallet_id} chain {chain_id} ({})",
            result.source
        ),
        None,
        true,
    );
    Ok(result)
}

#[derive(Serialize)]
pub struct PnlLastScan {
    pub wallet_id: i64,
    pub chain_id: i64,
    pub payload: String,
    pub status: String,
    pub scanned_at: i64,
}

pub fn last_scan(wallet_id: i64) -> AppResult<Option<PnlLastScan>> {
    crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT wallet_id, chain_id, payload, status, scanned_at
             FROM pnl_scans WHERE wallet_id = ?1 ORDER BY scanned_at DESC LIMIT 1",
        )?;
        let mut rows = stmt.query([wallet_id])?;
        if let Some(row) = rows.next()? {
            Ok(Some(PnlLastScan {
                wallet_id: row.get(0)?,
                chain_id: row.get(1)?,
                payload: row.get(2)?,
                status: row.get(3)?,
                scanned_at: row.get(4)?,
            }))
        } else {
            Ok(None)
        }
    })
}

#[derive(Serialize)]
pub struct PnlHistoryRow {
    pub id: i64,
    pub wallet_id: i64,
    pub chain_id: i64,
    pub status: String,
    pub scanned_at: i64,
    pub summary: serde_json::Value,
}

pub fn history(wallet_id: i64, limit: u64) -> AppResult<Vec<PnlHistoryRow>> {
    crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, wallet_id, chain_id, payload, status, scanned_at
             FROM pnl_scans WHERE wallet_id = ?1 ORDER BY scanned_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(rusqlite::params![wallet_id, limit as i64], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, wid, cid, payload, status, scanned_at) = row?;
            let summary = serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
            out.push(PnlHistoryRow {
                id,
                wallet_id: wid,
                chain_id: cid,
                status,
                scanned_at,
                summary,
            });
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_units_whole_and_frac() {
        assert_eq!(format_units(U256::from(1_000_000_000_000_000_000u64), 18), "1");
        assert_eq!(format_units(U256::from(1_500_000_000_000_000_000u64), 18), "1.5");
        assert_eq!(format_units(U256::from(123u64), 6), "0.000123");
        assert_eq!(format_units(U256::ZERO, 18), "0");
    }

    #[test]
    fn signed_flow_directions() {
        let f = |i: u64, o: u64| signed_flow(U256::from(i), U256::from(o));
        assert_eq!(f(2_000_000_000_000_000_000, 500_000_000_000_000_000), "+1.5");
        assert_eq!(f(0, 3_000_000_000_000_000_000), "-3");
        assert_eq!(f(1, 1), "0");
    }

    #[test]
    fn parse_erc20_flows_filters_3_topics() {
        let wallet = "0x2222222222222222222222222222222222222222";
        let erc20 = serde_json::json!({
            "address": "0xabc0000000000000000000000000000000000001",
            "topics": [
                TRANSFER_TOPIC,
                "0x0000000000000000000000001111111111111111111111111111111111111111",
                "0x0000000000000000000000002222222222222222222222222222222222222222"
            ],
            "data": format!("0x{:064x}", 1_000_000u64)
        });
        // 4-topic (ERC-721) must be ignored
        let erc721 = serde_json::json!({
            "address": "0xabc0000000000000000000000000000000000002",
            "topics": [
                TRANSFER_TOPIC,
                "0x0000000000000000000000001111111111111111111111111111111111111111",
                "0x0000000000000000000000002222222222222222222222222222222222222222",
                "0x000000000000000000000000000000000000000000000000000000000000002a"
            ],
            "data": "0x"
        });
        let out = parse_erc20_flows(&[erc20, erc721], wallet, false);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, U256::from(1_000_000u64));
    }

    #[test]
    fn short_contract_keeps_ends() {
        let c = "0xe1dd28bb9c61dac72f7a47f0a2c712eb4976ec2f";
        let s = short_contract(c);
        assert!(s.starts_with("0xe1dd2"));
        assert!(s.ends_with("ec2f"));
    }
}
