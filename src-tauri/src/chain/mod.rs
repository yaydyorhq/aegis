use crate::error::{AppError, AppResult};
use crate::wallet_store;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use std::sync::OnceLock;

pub mod eligibility;
pub mod erc20;
pub mod nft;

#[derive(Serialize, Deserialize, Clone)]
pub struct ChainRow {
    pub id: i64,
    pub name: String,
    pub chain_id: i64,
    pub rpc_url: String,
    pub symbol: String,
    pub explorer: Option<String>,
    pub enabled: i64,
}

#[derive(Deserialize)]
pub struct ChainInput {
    pub id: Option<i64>,
    pub name: String,
    pub chain_id: i64,
    pub rpc_url: String,
    pub symbol: String,
    pub explorer: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Serialize)]
pub struct RpcTestResult {
    pub ok: bool,
    pub chain_id_returned: Option<i64>,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
    /// Whether this endpoint supports `eth_call` (contract interaction).
    /// Free gateways like Cloudflare sometimes fail this — indicates the
    /// endpoint will break on mint tasks.
    pub eth_call_ok: Option<bool>,
}

pub fn list_chains() -> AppResult<Vec<ChainRow>> {
    crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, chain_id, rpc_url, symbol, explorer, enabled
             FROM chains ORDER BY enabled DESC, name",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(ChainRow {
                id: r.get(0)?,
                name: r.get(1)?,
                chain_id: r.get(2)?,
                rpc_url: r.get(3)?,
                symbol: r.get(4)?,
                explorer: r.get(5)?,
                enabled: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn upsert_chain(c: &ChainInput) -> AppResult<ChainRow> {
    if c.name.trim().is_empty() {
        return Err(AppError::Invalid("chain name required".into()));
    }
    if !c.rpc_url.starts_with("http://") && !c.rpc_url.starts_with("https://") {
        return Err(AppError::Invalid("rpc_url must be http(s)".into()));
    }
    if c.chain_id <= 0 {
        return Err(AppError::Invalid("chain_id must be positive".into()));
    }
    let enabled = c.enabled.unwrap_or(true) as i64;
    crate::db::with_conn(|conn| {
        if let Some(id) = c.id {
            let n = conn.execute(
                "UPDATE chains SET name=?1, chain_id=?2, rpc_url=?3, symbol=?4, explorer=?5, enabled=?6 WHERE id=?7",
                rusqlite::params![c.name.trim(), c.chain_id, c.rpc_url.trim(), c.symbol.trim(), c.explorer, enabled, id],
            )?;
            if n == 0 {
                return Err(AppError::NotFound(format!("chain {id}")));
            }
        } else {
            // Insert; if chain_id already exists, update that row instead of
            // failing on UNIQUE(chain_id) — friendlier "add/edit by chain id".
            let n = conn.execute(
                "INSERT INTO chains(name, chain_id, rpc_url, symbol, explorer, enabled)
                 VALUES (?1,?2,?3,?4,?5,?6)
                 ON CONFLICT(chain_id) DO UPDATE SET
                   name=excluded.name,
                   rpc_url=excluded.rpc_url,
                   symbol=excluded.symbol,
                   explorer=excluded.explorer,
                   enabled=excluded.enabled",
                rusqlite::params![c.name.trim(), c.chain_id, c.rpc_url.trim(), c.symbol.trim(), c.explorer, enabled],
            )?;
            let _ = n;
        }
        Ok(())
    })?;
    wallet_store::log_activity(
        "chain.upsert",
        &format!("Saved chain {} ({})", c.name, c.chain_id),
        None,
        true,
    );
    list_chains()?
        .into_iter()
        .find(|x| x.chain_id == c.chain_id)
        .ok_or_else(|| AppError::NotFound("chain".into()))
}

pub fn delete_chain(id: i64) -> AppResult<()> {
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute("DELETE FROM chains WHERE id = ?1", [id])?)
    })?;
    if n == 0 {
        return Err(AppError::NotFound(format!("chain {id}")));
    }
    wallet_store::log_activity("chain.delete", &format!("Deleted chain #{id}"), None, true);
    Ok(())
}

/// One shared client for every JSON-RPC call: a client per call meant a fresh
/// TCP+TLS handshake on each request, which is the dominant cost inside a
/// launch second. Keep-alive keeps the pool warm between the prep pass and the
/// fire path (OSNM-Z's `warm_submission_endpoint` relies on the same reuse).
static RPC_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();

fn rpc_client() -> &'static reqwest::Client {
    RPC_CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(12))
            .user_agent(concat!("aegis/", env!("CARGO_PKG_VERSION")))
            .pool_max_idle_per_host(8)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            .build()
            .expect("rpc client")
    })
}

pub async fn rpc_call(rpc_url: &str, method: &str, params: serde_json::Value) -> AppResult<serde_json::Value> {
    let client = rpc_client();
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });

    let mut attempt = 0u32;
    loop {
        let attempt_result: AppResult<serde_json::Value> = async {
            let resp = client
                .post(rpc_url)
                .json(&body)
                .send()
                .await
                .map_err(|e| AppError::Rpc(redact_urls_in(&e.to_string())))?;
            let status = resp.status();
            if is_transient_status(status.as_u16()) {
                return Err(AppError::Rpc(format!("HTTP {status}")));
            }
            if !status.is_success() {
                return Err(AppError::Rpc(format!("HTTP {status}")));
            }
            let v: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| AppError::Rpc(redact_urls_in(&e.to_string())))?;
            if let Some(err) = v.get("error") {
                return Err(AppError::Rpc(err.to_string()));
            }
            Ok(v)
        }
        .await;

        match attempt_result {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = match &e {
                    AppError::Rpc(msg) => {
                        msg.starts_with("HTTP 429")
                            || msg.starts_with("HTTP 5")
                            || !msg.starts_with("HTTP ")
                    }
                    _ => false,
                };
                if !retryable || attempt >= RPC_MAX_ATTEMPTS {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(backoff_delay_ms(attempt))).await;
                attempt += 1;
            }
        }
    }
}

/// One endpoint's outcome as fed to [`evaluate_chain_probes`].
struct EndpointOutcome {
    /// `Err` = the `eth_chainId` call itself failed (transport/parse; message
    /// is display-ready). `Ok` = the id the node returned.
    chain_id: Result<i64, String>,
    call_ok: bool,
    latency_ms: u64,
}

/// Fold per-endpoint probe results into the command verdict: the first
/// endpoint that both reports the configured chain id *and* passes the
/// eth_call capability probe wins; otherwise every failure is joined into one
/// error with URLs redacted. A dead provider in the list can no longer fail a
/// chain whose other endpoint is healthy.
fn evaluate_chain_probes(
    expected: i64,
    outcomes: &[(String, EndpointOutcome)],
) -> RpcTestResult {
    if let Some((_, pass)) = outcomes
        .iter()
        .find(|(_, o)| matches!(&o.chain_id, Ok(cid) if *cid == expected && o.call_ok))
    {
        return RpcTestResult {
            ok: true,
            chain_id_returned: pass.chain_id.as_ref().ok().copied(),
            latency_ms: Some(pass.latency_ms),
            error: None,
            eth_call_ok: Some(true),
        };
    }
    let mut failures = Vec::new();
    let mut last_cid = None;
    for (url, o) in outcomes {
        let why = match &o.chain_id {
            Ok(cid) => {
                last_cid = Some(*cid);
                if *cid != expected {
                    format!("expected {expected}, got {cid}")
                } else {
                    "eth_call capability missing (this RPC may fail on mint tasks)".to_string()
                }
            }
            Err(e) => e.clone(),
        };
        failures.push(format!("{why} @ {url}"));
    }
    let any_call_ok = outcomes
        .iter()
        .any(|(_, o)| matches!(&o.chain_id, Ok(_)) && o.call_ok);
    RpcTestResult {
        ok: false,
        chain_id_returned: last_cid,
        latency_ms: Some(
            outcomes
                .iter()
                .map(|(_, o)| o.latency_ms)
                .max()
                .unwrap_or(0),
        ),
        error: Some(if failures.is_empty() {
            "no endpoint configured".to_string()
        } else {
            redact_urls_in(&failures.join(" | "))
        }),
        eth_call_ok: Some(any_call_ok),
    }
}

pub async fn test_chain(id: i64) -> AppResult<RpcTestResult> {
    let chain = list_chains()?
        .into_iter()
        .find(|c| c.id == id)
        .ok_or_else(|| AppError::NotFound(format!("chain {id}")))?;
    // Probe every listed endpoint in order and stop at the first that passes;
    // a single dead/misconfigured provider must not fail the whole chain.
    let mut outcomes: Vec<(String, EndpointOutcome)> = Vec::new();
    for url in endpoint_list(&chain.rpc_url) {
        let start = std::time::Instant::now();
        let (chain_id, call_ok, latency_ms) = match eth_chain_id(&url).await {
            Ok(cid) => {
                let first = start.elapsed().as_millis() as u64;
                // Probe eth_call capability — catches Cloudflare/gateway nodes
                // that accept eth_chainId but fail on contract interaction.
                let (probe_cid, _blk, call_ok, probe_latency, _probe_err) =
                    rpc_health_probe(&url).await;
                let latency = probe_latency.max(first);
                // Prefer the detailed first answer; the probe re-asks and may
                // have lost a race with a flapping gateway.
                (Ok(probe_cid.unwrap_or(cid)), call_ok, latency)
            }
            Err(e) => (Err(e.to_string()), false, start.elapsed().as_millis() as u64),
        };
        outcomes.push((
            url,
            EndpointOutcome {
                chain_id,
                call_ok,
                latency_ms,
            },
        ));
        let early = evaluate_chain_probes(chain.chain_id, &outcomes);
        if early.ok {
            wallet_store::log_activity(
                "rpc.test",
                &format!(
                    "{} RPC OK ({} ms, eth_call ok)",
                    chain.name,
                    early.latency_ms.unwrap_or(0)
                ),
                None,
                true,
            );
            return Ok(early);
        }
    }
    let result = evaluate_chain_probes(chain.chain_id, &outcomes);
    wallet_store::log_activity(
        "rpc.test",
        &format!(
            "{} RPC failed: {}",
            chain.name,
            result.error.as_deref().unwrap_or("unknown")
        ),
        None,
        false,
    );
    Ok(result)
}

pub async fn eth_chain_id(rpc_url: &str) -> AppResult<i64> {
    let v = rpc_call(rpc_url, "eth_chainId", serde_json::json!([])).await?;
    let hex_id = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing result".into()))?;
    i64::from_str_radix(hex_id.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad chainId".into()))
}

/// Light capability probe: eth_chainId + eth_blockNumber + a trivial
/// eth_call (ERC20 totalSupply selector 0x18160ddd on the zero address).
/// Returns (chain_id, block_number_ms, eth_call_ok, latency_ms, error_msg).
pub async fn rpc_health_probe(url: &str) -> (Option<i64>, Option<u64>, bool, u64, Option<String>) {
    let start = std::time::Instant::now();

    // 1. chainId
    let chain_id = eth_chain_id(url).await.ok();

    // 2. blockNumber
    let block_number = match rpc_call(url, "eth_blockNumber", serde_json::json!([])).await {
        Ok(v) => v
            .get("result")
            .and_then(|r| r.as_str())
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
        Err(_) => None,
    };

    // 3. eth_call — the test that Cloudflare (and other lightweight gateways)
    //    fail.  We call `totalSupply()` on the zero address; the revert is
    //    expected — what we check is whether the node *tries* to execute it
    //    (returns error code 3 = revert) instead of crashing with -32603.
    let call_ok = match rpc_call(
        url,
        "eth_call",
        serde_json::json!([{ "to": "0x0000000000000000000000000000000000000000", "data": "0x18160ddd" }, "latest"]),
    )
    .await
    {
        Ok(v) => {
            // A revert (code 3) means the node executed the call — that's OK.
            // -32603 / "Internal error" means it could not even try.
            let is_internal = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .map(|m| m.contains("Internal error") || m.contains("-32603"))
                .unwrap_or(false);
            !is_internal
        }
        Err(_) => false,
    };
    let latency_ms = start.elapsed().as_millis() as u64;

    (
        chain_id,
        block_number,
        call_ok,
        latency_ms,
        if !call_ok {
            Some("eth_call capability missing (this RPC may fail on mint tasks — add another endpoint as fallback)".into())
        } else if chain_id.is_none() {
            Some("chainId unreachable".into())
        } else {
            None
        },
    )
}
pub async fn native_balance(rpc_url: &str, address: &str) -> AppResult<String> {
    let v = rpc_call(
        rpc_url,
        "eth_getBalance",
        serde_json::json!([address, "latest"]),
    )
    .await?;
    v.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Rpc("missing balance".into()))
}

pub async fn block_number(rpc_url: &str) -> AppResult<u64> {
    let v = rpc_call(rpc_url, "eth_blockNumber", serde_json::json!([])).await?;
    let hex_id = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing blockNumber".into()))?;
    u64::from_str_radix(hex_id.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad block".into()))
}

pub async fn get_logs(
    rpc_url: &str,
    from_block: u64,
    to_block: u64,
    address: Option<&str>,
    topics: Vec<Option<String>>,
) -> AppResult<serde_json::Value> {
    let topics = topics
        .into_iter()
        .map(|t| match t {
            Some(s) => serde_json::json!(s),
            None => serde_json::Value::Null,
        })
        .collect();
    get_logs_value(rpc_url, from_block, to_block, address, topics).await
}

/// eth_getLogs with raw topic values (string | array | null) — supports
/// OR-lists (`[addrA, addrB]`) that Option<String> topics cannot express.
pub async fn get_logs_value(
    rpc_url: &str,
    from_block: u64,
    to_block: u64,
    address: Option<&str>,
    topics: Vec<serde_json::Value>,
) -> AppResult<serde_json::Value> {
    let mut filter = serde_json::json!({
        "fromBlock": format!("0x{:x}", from_block),
        "toBlock": format!("0x{:x}", to_block),
    });
    if let Some(a) = address {
        filter["address"] = serde_json::json!(a);
    }
    if !topics.is_empty() {
        filter["topics"] = serde_json::Value::Array(topics);
    }
    let v = rpc_call(rpc_url, "eth_getLogs", serde_json::json!([filter])).await?;
    Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

/// RPC rejected the block range (result/size limits) — the adaptive fetcher
/// should halve its span. Transient HTTP errors (429/5xx) are NOT matched so
/// they stay with rpc_call's retry/backoff.
pub fn is_range_error(e: &AppError) -> bool {
    let s = e.to_string().to_lowercase();
    [
        "range",
        "too large",
        "too many",
        "more than",
        "exceed",
        "max results",
        "query returned",
        "block span",
    ]
    .iter()
    .any(|k| s.contains(k))
}

/// eth_getLogs over [from..to] that tries the whole range in one request
/// (fast chains allow 70M+ blocks) and halves the span when the RPC enforces
/// range/result limits. `on_tick(scanned_blocks, total_blocks)` fires after
/// each successful chunk.
pub async fn get_logs_adaptive(
    rpc_url: &str,
    from_block: u64,
    to_block: u64,
    address: Option<&str>,
    topics: Vec<serde_json::Value>,
    on_tick: Option<&(dyn Fn(u64, u64) + Sync)>,
) -> AppResult<Vec<serde_json::Value>> {
    let mut out: Vec<serde_json::Value> = Vec::new();
    if to_block < from_block {
        return Ok(out);
    }
    let total = to_block - from_block + 1;
    let min_span = 2_000u64;
    let mut start = from_block;
    let mut span = total;
    let mut scanned = 0u64;
    while start <= to_block {
        let end = (start + span - 1).min(to_block);
        match get_logs_value(rpc_url, start, end, address, topics.clone()).await {
            Ok(res) => {
                if let Some(arr) = res.as_array() {
                    out.extend(arr.iter().cloned());
                }
                scanned += end - start + 1;
                if let Some(t) = on_tick {
                    t(scanned, total);
                }
                start = end + 1;
            }
            Err(e) if is_range_error(&e) && (end - start + 1) > min_span => {
                span = ((end - start + 1) / 2).max(min_span);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Split a chain row's `rpc_url` into individual endpoints (comma or
/// whitespace separated) and trim empties. Pollers rotate over the list so a
/// single dead provider cannot blind a receipt check.
pub fn endpoint_list(rpc_url: &str) -> Vec<String> {
    let out: Vec<String> = rpc_url
        .split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if out.is_empty() {
        vec![rpc_url.to_string()]
    } else {
        out
    }
}

pub async fn get_transaction_count(rpc_url: &str, address: &str) -> AppResult<u64> {
    let v = rpc_call(
        rpc_url,
        "eth_getTransactionCount",
        serde_json::json!([address, "pending"]),
    )
    .await?;
    let hex_id = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing nonce".into()))?;
    u64::from_str_radix(hex_id.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad nonce".into()))
}

pub async fn estimate_gas(rpc_url: &str, tx: serde_json::Value) -> AppResult<u64> {
    let v = rpc_call(rpc_url, "eth_estimateGas", serde_json::json!([tx])).await?;
    let hex_id = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing gas".into()))?;
    u64::from_str_radix(hex_id.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad gas".into()))
}

pub async fn gas_price(rpc_url: &str) -> AppResult<u128> {
    let v = rpc_call(rpc_url, "eth_gasPrice", serde_json::json!([])).await?;
    let hex_id = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing gasPrice".into()))?;
    u128::from_str_radix(hex_id.trim_start_matches("0x"), 16)
        .map_err(|_| AppError::Rpc("bad gasPrice".into()))
}

pub async fn send_raw_transaction(rpc_url: &str, raw: &str) -> AppResult<String> {
    let v = rpc_call(
        rpc_url,
        "eth_sendRawTransaction",
        serde_json::json!([raw]),
    )
    .await?;
    v.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Rpc("missing tx hash".into()))
}

pub async fn get_receipt(rpc_url: &str, tx_hash: &str) -> AppResult<Option<serde_json::Value>> {
    let v = rpc_call(rpc_url, "eth_getTransactionReceipt", serde_json::json!([tx_hash])).await?;
    let result = v.get("result").cloned().unwrap_or(serde_json::Value::Null);
    if result.is_null() {
        Ok(None)
    } else {
        Ok(Some(result))
    }
}

const RPC_MAX_ATTEMPTS: u32 = 3;

/// Replace every http(s) URL in `msg` with `scheme://host/...`.
///
/// RPC endpoints routinely carry API keys in the path (Alchemy, Infura) and
/// reqwest's transport errors append the full URL to their Display output.
/// Anything logged or persisted must go through this first — the app's own
/// `api_keys` table encrypts keys at rest, so leaking one via a log row would
/// defeat that.
pub fn redact_urls_in(msg: &str) -> String {
    const MARKERS: [&str; 2] = ["https://", "http://"];
    const STOP: [char; 8] = [' ', ')', '\'', '"', ',', '\n', '\t', '`'];
    let mut out = String::with_capacity(msg.len());
    let mut rest = msg;
    loop {
        let mut best: Option<(usize, &'static str)> = None;
        for m in MARKERS {
            if let Some(i) = rest.find(m) {
                if best.map(|(b, _)| i < b).unwrap_or(true) {
                    best = Some((i, m));
                }
            }
        }
        let Some((start, scheme)) = best else {
            out.push_str(rest);
            break;
        };
        let after = &rest[start + scheme.len()..];
        let end = after
            .find(|c: char| STOP.contains(&c))
            .unwrap_or(after.len());
        let url_end = start + scheme.len() + end;
        let body = &rest[start + scheme.len()..url_end];
        let host_end = body.find('/').unwrap_or(body.len());
        out.push_str(&rest[..start]);
        out.push_str(scheme);
        out.push_str(&body[..host_end]);
        out.push_str("/...");
        rest = &rest[url_end..];
    }
    out
}

/// Single-URL form: `https://eth-mainnet.g.alchemy.com/v2/SECRET` →
/// `https://eth-mainnet.g.alchemy.com/...`
pub fn redact_rpc(url: &str) -> String {
    redact_urls_in(url)
}

fn is_transient_status(status: u16) -> bool {
    status == 429 || (500..=599).contains(&status)
}

fn backoff_delay_ms(attempt: u32) -> u64 {
    1_000u64 << attempt.min(3)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_statuses_are_429_and_5xx() {
        assert!(is_transient_status(429));
        assert!(is_transient_status(500));
        assert!(is_transient_status(502));
        assert!(is_transient_status(503));
        assert!(!is_transient_status(400));
        assert!(!is_transient_status(401));
        assert!(!is_transient_status(404));
    }

    #[test]
    fn backoff_delays_grow_exponentially() {
        assert_eq!(backoff_delay_ms(0), 1_000);
        assert_eq!(backoff_delay_ms(1), 2_000);
        assert_eq!(backoff_delay_ms(2), 4_000);
        assert_eq!(backoff_delay_ms(3), 8_000);
    }

    #[test]
    fn endpoint_list_splits_and_trims() {
        assert_eq!(
            endpoint_list("https://a.example, https://b.example"),
            vec!["https://a.example".to_string(), "https://b.example".to_string()]
        );
        assert_eq!(endpoint_list(" https://c.example ,"), vec!["https://c.example".to_string()]);
        assert_eq!(endpoint_list("https://d.example"), vec!["https://d.example".to_string()]);
        // Empty input still yields something pollable instead of a panic.
        assert_eq!(endpoint_list(""), vec!["".to_string()]);
        assert_eq!(endpoint_list("  ,\n ").len(), 1); // degenerate input
    }

    fn passing(url: &str, latency: u64) -> (String, EndpointOutcome) {
        (
            url.to_string(),
            EndpointOutcome {
                chain_id: Ok(8453),
                call_ok: true,
                latency_ms: latency,
            },
        )
    }

    /// The false-negative the audit flagged: endpoint #1 is down, #2 is fine —
    /// the chain must test OK with the healthy endpoint's latency.
    #[test]
    fn chain_test_survives_a_dead_first_endpoint() {
        let outcomes = vec![
            (
                "https://dead.example/v2/KEY".to_string(),
                EndpointOutcome {
                    chain_id: Err("HTTP 502".into()),
                    call_ok: false,
                    latency_ms: 300,
                },
            ),
            passing("http://100.79.165.20:8549", 95),
        ];
        let r = evaluate_chain_probes(8453, &outcomes);
        assert!(r.ok);
        assert_eq!(r.chain_id_returned, Some(8453));
        assert_eq!(r.latency_ms, Some(95));
        assert_eq!(r.error, None);
        assert_eq!(r.eth_call_ok, Some(true));
    }

    #[test]
    fn chain_test_reports_chain_id_mismatch_with_redacted_url() {
        let outcomes = vec![(
            "https://rpc.example.com/v2/SUPERSECRET".to_string(),
            EndpointOutcome {
                chain_id: Ok(1),
                call_ok: true,
                latency_ms: 42,
            },
        )];
        let r = evaluate_chain_probes(8453, &outcomes);
        assert!(!r.ok);
        let e = r.error.expect("error message");
        assert!(e.contains("expected 8453, got 1"), "{e}");
        assert!(!e.contains("SUPERSECRET"), "key leaked: {e}");
        assert!(
            e.contains("https://rpc.example.com/..."),
            "url not redacted: {e}"
        );
        assert_eq!(r.chain_id_returned, Some(1));
        assert_eq!(r.eth_call_ok, Some(true)); // call works — chain is wrong
    }

    #[test]
    fn chain_test_all_unreachable_is_reported_with_transport_errors() {
        let outcomes = vec![(
            "http://100.79.165.20:8548".to_string(),
            EndpointOutcome {
                chain_id: Err("HTTP 429".into()),
                call_ok: false,
                latency_ms: 7,
            },
        )];
        let r = evaluate_chain_probes(1, &outcomes);
        assert!(!r.ok);
        assert_eq!(r.chain_id_returned, None);
        assert_eq!(r.eth_call_ok, Some(false));
        assert!(r.error.unwrap().contains("HTTP 429"));
    }

    #[test]
    fn chain_test_eth_call_gap_marks_the_chain_unfit() {
        let outcomes = vec![(
            "https://ok.example".to_string(),
            EndpointOutcome {
                chain_id: Ok(8453),
                call_ok: false,
                latency_ms: 10,
            },
        )];
        let r = evaluate_chain_probes(8453, &outcomes);
        assert!(!r.ok);
        assert_eq!(r.eth_call_ok, Some(false));
        assert!(r.error.unwrap().contains("eth_call"));
    }

    #[test]
    fn redact_strips_api_keys_from_rpc_urls() {
        assert_eq!(
            redact_rpc("https://eth-mainnet.g.alchemy.com/v2/alch_SUPERSECRET"),
            "https://eth-mainnet.g.alchemy.com/..."
        );
        // Host (and port) survive; only the path is dropped.
        assert_eq!(redact_rpc("http://100.79.165.20:8547"), "http://100.79.165.20:8547/...");
    }

    #[test]
    fn redact_covers_urls_embedded_in_messages() {
        let msg = "error sending request for url (https://rpc.example.com/v2/KEY123): timed out";
        let out = redact_urls_in(msg);
        assert!(!out.contains("KEY123"), "key leaked: {out}");
        assert!(out.contains("https://rpc.example.com/..."), "lost url: {out}");
        assert!(out.contains("timed out"), "lost suffix: {out}");

        // Two URLs in one string, mixed with non-url text.
        let two = "failed over from https://a.io/v1/x to https://b.io/v2/y after 429";
        let out2 = redact_urls_in(two);
        assert!(!out2.contains("/v1/x") && !out2.contains("/v2/y"), "{out2}");
        assert!(out2.contains("https://a.io/...") && out2.contains("https://b.io/..."), "{out2}");
    }

    #[test]
    fn redact_is_a_noop_without_urls() {
        assert_eq!(redact_urls_in("tx 0xabc reverted"), "tx 0xabc reverted");
        assert_eq!(redact_urls_in(""), "");
    }
}
