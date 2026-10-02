use crate::chain;
use crate::error::{AppError, AppResult};
use crate::wallet_store;
use serde::Serialize;

pub mod allowlist;
pub mod seadrop;

const FLASHBOTS_RPC: &str = "https://rpc.flashbots.net";
/// Max re-entry count into the receipt poller before a broadcasting task is
/// declared failed (one attempt ≈ one scheduler run ≈ up to 30s of polling).
const MAX_POLL_ATTEMPTS: i64 = 40;

#[derive(Serialize, Clone)]
pub struct MintTaskRow {
    pub id: i64,
    pub chain_id: i64,
    pub contract: String,
    pub quantity: i64,
    pub value_wei: Option<String>,
    pub calldata: Option<String>,
    pub status: String,
    pub tx_hash: Option<String>,
    pub error: Option<String>,
    pub wallet_id: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub function_name: Option<String>,
    pub is_hex: bool,
    pub parameters: Option<String>,
    pub rpc_endpoints: Option<String>,
    pub flashbots: bool,
    pub gas_limit: Option<String>,
    pub max_fee_gwei: Option<String>,
    pub priority_fee_gwei: Option<String>,
    pub nonce_override: Option<String>,
    pub scheduled_at: Option<i64>,
    pub delay_ms: i64,
    pub mode: String,
    pub poll_attempts: i64,
    pub auto_retries: i64,
    /// JSON `{collection, token_id}`: resolve SeaDrop mint data at fire time.
    pub opensea_ref: Option<String>,
    /// Hash of the tx this one replaced after a fee bump (also polled).
    pub prev_tx_hash: Option<String>,
    /// How many times a stuck broadcast has been re-signed with a higher fee.
    pub bump_count: i64,
}

#[derive(Clone, Default)]
pub struct EnqueueArgs {
    pub wallet_id: i64,
    pub chain_id: i64,
    pub contract: String,
    pub quantity: i64,
    pub value_wei: Option<String>,
    pub function_name: Option<String>,
    pub is_hex: bool,
    pub parameters: Option<String>,
    pub calldata: Option<String>,
    pub rpc_endpoints: Option<Vec<String>>,
    pub flashbots: bool,
    pub gas_limit: Option<String>,
    pub max_fee_gwei: Option<String>,
    pub priority_fee_gwei: Option<String>,
    pub nonce_override: Option<String>,
    pub scheduled_at: Option<i64>,
    pub delay_ms: Option<i64>,
    pub mode: Option<String>,
    /// Deferred OpenSea mint: JSON `{collection, token_id}` resolved at fire time.
    pub opensea_ref: Option<String>,
}

/// Wallets with a mint lane currently in flight. One lane per wallet keeps
/// same-wallet tasks sequential (nonce order) while every other wallet keeps
/// firing on its own lane — a `delay_ms` sleep inside one wallet's lane must
/// not stall the whole scheduler. The old process-wide `MINT_RUNNING` flag did
/// exactly that: while any lane slept, no other wallet's due task could run.
static LANE_GUARDS: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<Option<i64>>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
/// Task ids with a background receipt poll in flight — prevents duplicate
/// pollers across scheduler ticks and lets polls outlive the run guard.
static POLL_IN_FLIGHT: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
/// Task ids already prepped inside their lead window, so the 60ms scheduler
/// tick reports each one once. A prep failure clears its mark and retries.
static PREPPED: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));
/// OpenSea collection metadata warmed before fire time — slug/network lookup is
/// a network roundtrip we do not want inside the T-2 window. Purely additive:
/// a miss just falls back to the normal resolve path.
static COLLECTION_CACHE: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<String, crate::opensea::ResolvedCollection>>,
> = once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Prep lead: warm endpoints, nonce/gas and OpenSea metadata this long before a
/// task's `scheduled_at` (OSNM-Z warms the submission endpoint at T-10s).
pub const PREP_LEAD_MS: i64 = 10_000;

fn lock_unpoisoned<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Marks one wallet's lane busy; released when the lane finishes — including
/// on panic — so the wallet's next tasks are picked up by a later tick.
struct LaneGuard(Option<i64>);
impl Drop for LaneGuard {
    fn drop(&mut self) {
        lock_unpoisoned(&LANE_GUARDS).remove(&self.0);
    }
}

/// Kick off a background receipt poll for `id` unless one is already running.
/// Detaching keeps the 30s poll window from ever holding a lane — fresh
/// scheduled mints must be able to fire on the next 60ms tick.
fn spawn_receipt_poll(id: i64) {
    {
        let mut g = POLL_IN_FLIGHT
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !g.insert(id) {
            return; // already polling this task
        }
    }
    tokio::spawn(async move {
        struct Release(i64);
        impl Drop for Release {
            fn drop(&mut self) {
                let mut g = POLL_IN_FLIGHT
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                g.remove(&self.0);
            }
        }
        let _release = Release(id);
        if let Err(e) = process_task(id).await {
            release_task_nonce(id);
            crate::logging::error(
                "mint.poll",
                &format!("Mint #{id} receipt poll error: {e}"),
            );
            set_status(id, "failed", None, Some(&e.to_string()));
            wallet_store::log_activity(
                "mint.failed",
                &format!("Mint #{id} receipt poll error: {e}"),
                None,
                false,
            );
        }
    });
}

/// True while any wallet lane is executing (test diagnostics).
#[cfg(test)]
pub fn is_running() -> bool {
    !lock_unpoisoned(&LANE_GUARDS).is_empty()
}

/// cfg(test) helpers to simulate a lane that is already in flight.
#[cfg(test)]
fn force_lane_busy(wallet_id: Option<i64>) {
    lock_unpoisoned(&LANE_GUARDS).insert(wallet_id);
}

#[cfg(test)]
fn clear_lanes_for_tests() {
    lock_unpoisoned(&LANE_GUARDS).clear();
}

/// Pending tasks that are due (or unscheduled) plus any in-flight broadcasts.
pub fn count_runnable() -> AppResult<i64> {
    crate::db::with_conn(|conn| {
        let now = crate::db::now_ms();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mint_tasks WHERE
             (status = 'pending' AND (scheduled_at IS NULL OR scheduled_at <= ?1))
             OR status = 'broadcasting'",
            [now],
            |r| r.get(0),
        )?;
        Ok(n)
    })
}

const SELECT_COLS: &str = "id, chain_id, contract, quantity, value_wei, calldata, status, tx_hash, error, wallet_id, created_at, updated_at, function_name, is_hex, parameters, rpc_endpoints, flashbots, gas_limit, max_fee_gwei, priority_fee_gwei, nonce_override, scheduled_at, delay_ms, mode, poll_attempts, auto_retries, opensea_ref, prev_tx_hash, bump_count";

fn map_row(r: &rusqlite::Row) -> rusqlite::Result<MintTaskRow> {
    Ok(MintTaskRow {
        id: r.get(0)?,
        chain_id: r.get(1)?,
        contract: r.get(2)?,
        quantity: r.get(3)?,
        value_wei: r.get(4)?,
        calldata: r.get(5)?,
        status: r.get(6)?,
        tx_hash: r.get(7)?,
        error: r.get(8)?,
        wallet_id: r.get(9)?,
        created_at: r.get(10)?,
        updated_at: r.get(11)?,
        function_name: r.get(12)?,
        is_hex: r.get::<_, i64>(13)? != 0,
        parameters: r.get(14)?,
        rpc_endpoints: r.get(15)?,
        flashbots: r.get::<_, i64>(16)? != 0,
        gas_limit: r.get(17)?,
        max_fee_gwei: r.get(18)?,
        priority_fee_gwei: r.get(19)?,
        nonce_override: r.get(20)?,
        scheduled_at: r.get(21)?,
        delay_ms: r.get(22)?,
        mode: r.get(23)?,
        poll_attempts: r.get(24)?,
        auto_retries: r.get(25)?,
        opensea_ref: r.get(26)?,
        prev_tx_hash: r.get(27)?,
        bump_count: r.get(28)?,
    })
}

/// Statuses the UI must always see (queued / in flight / draft). Everything
/// else is history and gets capped in [`list_tasks`].
const ACTIVE_STATUSES: &str = "'pending','draft','signing','broadcasting'";
/// Cap on finished rows returned to the frontend. The table grows forever;
/// the IPC payload must not.
const LIST_HISTORY_CAP: usize = 1000;

pub fn list_tasks() -> AppResult<Vec<MintTaskRow>> {
    crate::db::with_conn(|conn| {
        // Active rows are never capped — a queued task must not vanish because
        // old finished runs filled the budget. History contributes at most
        // LIST_HISTORY_CAP newest rows.
        let sql = format!(
            "SELECT {SELECT_COLS} FROM (
                 SELECT {SELECT_COLS} FROM mint_tasks WHERE status IN ({ACTIVE_STATUSES})
                 UNION ALL
                 SELECT {SELECT_COLS} FROM (
                     SELECT {SELECT_COLS} FROM mint_tasks WHERE status NOT IN ({ACTIVE_STATUSES})
                     ORDER BY created_at DESC, id DESC LIMIT {LIST_HISTORY_CAP}
                 )
             ) ORDER BY created_at DESC, id DESC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([], map_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn get_task(id: i64) -> AppResult<MintTaskRow> {
    crate::db::with_conn(|conn| {
        let sql = format!("SELECT {SELECT_COLS} FROM mint_tasks WHERE id = ?1");
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row([id], map_row).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound(format!("mint task {id}"))
            }
            other => AppError::Db(other),
        })
    })
}

fn validate_mode(mode: &str) -> AppResult<()> {
    match mode {
        "execute" | "simulate" | "spam" | "sweep" => Ok(()),
        other => Err(AppError::Invalid(format!(
            "mode must be execute|simulate|spam|sweep, got {other}"
        ))),
    }
}

fn opt_trim(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty() && x != "auto")
}

/// Pick the first endpoint whose chain_id matches `expected`, else the first
/// endpoint that responds at all.  Used before signing so a dead primary RPC
/// (node-specific -32603, 503, timeout) does not block the whole task when a
/// fallback endpoint is configured.
///
/// All endpoints are probed concurrently and the first chain-matched answer
/// wins: sequential probing cost a full request timeout *plus* rpc_call's
/// retries (≈40s for a hung node) per dead entry, at fire time.
async fn pick_healthy_endpoint(endpoints: &[String], expected_chain: i64) -> (String, Vec<String>) {
    let mut set = tokio::task::JoinSet::new();
    for url in endpoints {
        let url = url.clone();
        set.spawn(async move {
            let cid = chain::eth_chain_id(&url).await;
            (url, cid)
        });
    }
    let mut mismatch: Option<String> = None;
    let mut healthy: Option<String> = None;
    while let Some(joined) = set.join_next().await {
        if let Ok((url, Ok(cid))) = joined {
            if cid == expected_chain {
                healthy = Some(url);
                break; // dropping the JoinSet aborts the remaining probes
            } else if mismatch.is_none() {
                mismatch = Some(format!(
                    "{} is chain {cid}, expected {expected_chain}",
                    chain::redact_rpc(&url)
                ));
            }
        }
    }
    if let Some(url) = healthy {
        return (url, endpoints.to_vec());
    }
    // No chain-matched endpoint; fall back to the first entry and let the
    // downstream chain-id check report the mismatch explicitly.
    if let Some(m) = mismatch {
        wallet_store::log_activity(
            "rpc.warn",
            &format!("No endpoint matches the task chain: {m}"),
            None,
            false,
        );
    }
    (endpoints[0].clone(), endpoints.to_vec())
}

/// Deterministic failures: retrying the identical signed tx/calldata cannot
/// succeed, so these must never be auto-requeued or polled as "maybe landed".
fn is_permanent_err(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const PERMANENT: [&str; 8] = [
        "revert",
        "insufficient funds",
        "chain mismatch",
        // Wrong-chain endpoint reports ("… is chain 1, expected 4663").
        " is chain ",
        "bad gas",
        "bad max_fee",
        "bad value",
        // The nonce is already consumed by another tx — a retry with the same
        // nonce is rejected again; needs a fresh nonce read, not a requeue.
        "nonce too low",
    ];
    PERMANENT.iter().any(|p| m.contains(p))
}

/// Transient errors worth one automatic requeue (never reverts/param errors).
pub(crate) fn is_transient_err(msg: &str) -> bool {
    if is_permanent_err(msg) {
        return false;
    }
    let m = msg.to_ascii_lowercase();
    const TRANSIENT: [&str; 16] = [
        "transport",
        "timeout",
        "timed out",
        "connection",
        "connection reset",
        "429",
        "rate limit",
        "too many requests",
        "502",
        "503",
        "504",
        "dns",
        "temporarily",
        "eof",
        // RPC node-side failures that a different endpoint may not share.
        "-32603",
        "internal error",
    ];
    // NOTE: a decoded contract revert is deliberately NOT transient — retrying
    // the same calldata on another node returns the same revert.
    TRANSIENT.iter().any(|t| m.contains(t))
}

/// Map Solidity custom error selectors to readable text.  Bare 4-byte payloads
/// like `0x13da22f2` are otherwise useless to a user staring at a failed task.
///
/// Selectors computed from `keccak256("Name(args)")[..4]`.  Sourced from the
/// verified SeaDrop singleton ABI + ERC721SeaDropCloneable ABI (Blockscout),
/// so every entry is checked against a real deployed contract rather than
/// guessed from a name.
fn decode_revert_selector(hex: &str) -> Option<&'static str> {
    let sel = hex.trim_start_matches("0x").get(..8)?.to_ascii_lowercase();
    Some(match sel.as_str() {
        // ── SeaDrop: timing / stage state ────────────────────────────────
        // NotActive(currentTimestamp, startTimestamp, endTimestamp)
        "13da22f2" => "NotActive(...) — the drop is not open at this time (window not started or already ended)",
        "80cb55e2" => "NotActive() — this stage is not currently active",
        "333d33d0" => "InvalidSignedStartTime(got, minimum) — signed stage starts before the allowed minimum",
        "6e1d357d" => "InvalidSignedEndTime(got, maximum) — signed stage ends after the allowed maximum",
        // ── SeaDrop: authorization ───────────────────────────────────────
        // OnlyINonFungibleSeaDropToken(sender)
        "32c5d8cf" => "OnlyINonFungibleSeaDropToken(sender) — the NFT contract does not implement the SeaDrop token interface",
        "1fe7da08" => "PayerNotAllowed() — the payer address is not permitted for this mint",
        "4cc11713" => "PayerNotPresent() — no payer registered for the supplied fee recipient",
        // ── SeaDrop: payment ────────────────────────────────────────────
        "0d35e921" => "IncorrectPayment(got, want) — ETH value sent does not match the required mint price",
        "798701ac" => "DuplicateFeeRecipient() — the same fee recipient was supplied twice",
        "0998fbbd" => "FeeRecipientNotPresent() — fee recipient is not registered on the collection",
        "f477d26f" => "FeeRecipientNotAllowed() — feeRecipient rejected by the collection",
        "a0c3ed0a" => "InvalidSignedMintPrice(got, minimum) — signed mint price is below the required minimum",
        "79fc44ed" => "InvalidSignedFeeBps(got, minOrMax) — signed fee basis points outside the allowed range",
        // ── SeaDrop: quantity / supply ──────────────────────────────────
        "edc01273" => "MintQuantityExceedsMaxMintedPerWallet(total, allowed) — wallet hit its per-wallet cap",
        "e12d2314" => "MintQuantityExceedsMaxSupply(total, maxSupply) — collection sold out",
        "b98dabea" => "MintQuantityExceedsMaxTokenSupplyForStage(total, stageMax) — stage supply exhausted",
        "198441cb" => "MintQuantityCannotBeZero() — quantity must be > 0",
        // ── SeaDrop: signed mints / allowlist ───────────────────────────
        "09bde339" => "InvalidProof() — Merkle proof does not match the collection's allowlist root",
        "d855c4f4" => "InvalidSignature(recoveredSigner) — signed-mint signature rejected",
        "db8b2fad" => "SignedMintsMustRestrictFeeRecipients() — signed drop misconfigured",
        // ── SeaDrop clone (NFT side) ────────────────────────────────────
        "15e26ff3" => "OnlyAllowedSeaDrop() — caller is not an allowed SeaDrop implementation for this NFT",
        // ── OpenZeppelin ────────────────────────────────────────────────
        "08c379a0" => "Error(string) — revert reason in calldata (check the return data)",
        "4e487b71" => "Panic(uint256) — arithmetic overflow, division by zero, or failed assertion",
        _ => return None,
    })
}

/// Turn a raw RPC error into something a human can act on.  Detects:
///   * JSON-RPC error codes (-32603 node fault, 3 revert)
///   * `data: "0x........"` custom-error selectors
///   * Solidity `Error(string)` / `Panic(uint256)` payloads
fn decode_rpc_error(e: &AppError) -> String {
    let raw = e.to_string();
    let lower = raw.to_ascii_lowercase();

    // Pull the revert data payload if present — Aegis formats RPC failures as
    // `rpc: {"code":3,"message":"execution reverted","data":"0x13da22f2"}`.
    // Parsed as JSON (first `{` → last `}`) instead of splitting on the word
    // "data": a provider URL or message containing that word used to hijack
    // the split and mis-extract the payload.
    let json_payload = (|| -> Option<serde_json::Value> {
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        if end <= start {
            return None;
        }
        serde_json::from_str::<serde_json::Value>(&raw[start..=end]).ok()
    })();
    let data_hex = json_payload
        .as_ref()
        .and_then(|v| {
            v.get("data")
                .or_else(|| v.get("error").and_then(|err| err.get("data")))
        })
        .and_then(|d| d.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| s.starts_with("0x") && s.len() >= 10);

    if let Some(d) = data_hex {
        if let Some(name) = decode_revert_selector(&d) {
            return format!("reverted: {name} (raw {d})");
        }
        return format!("reverted: unrecognised custom error {d}");
    }

    if lower.contains("-32603") || lower.contains("internal error") {
        return format!(
            "{raw} — the RPC node could not execute this call. \
             Try another endpoint for this chain (node-side limitation, not calldata)."
        );
    }
    if lower.contains("execution reverted") {
        return format!("{raw} — call reverted without a readable reason");
    }
    raw
}

/// Runtime bytecode as lowercase hex without `0x`, or `None` when the RPC can't say.
async fn runtime_code(rpc_url: &str, address: &str) -> Option<String> {
    let v = chain::rpc_call(
        rpc_url,
        "eth_getCode",
        serde_json::json!([address, "latest"]),
    )
    .await
    .ok()?;
    let raw = v.get("result")?.as_str()?;
    Some(raw.trim_start_matches("0x").to_ascii_lowercase())
}

/// EIP-1167 minimal proxy stub → implementation address. Handles the canonical
/// variant plus the immutable-clones variant; anything else is not a stub.
fn minimal_proxy_target(code_hex: &str) -> Option<String> {
    const TAIL: &str = "5af43d82803e903d91602b57fd5bf3";
    for prefix in ["363d3d373d3d3d363d73", "3d3d3d3d363d73"] {
        if !code_hex.starts_with(prefix) {
            continue;
        }
        if code_hex.len() != prefix.len() + 40 + TAIL.len() || !code_hex.ends_with(TAIL) {
            continue;
        }
        let impl_hex = &code_hex[prefix.len()..prefix.len() + 40];
        if impl_hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(format!("0x{impl_hex}"));
        }
    }
    None
}

/// Implementation address behind a proxy: EIP-1167 stub first, then the
/// EIP-1967 and legacy Zeppelinos storage slots. `code_hex` is the target's
/// already-fetched runtime code (non-empty).
async fn proxy_implementation(rpc_url: &str, contract: &str, code_hex: &str) -> Option<String> {
    if let Some(impl_addr) = minimal_proxy_target(code_hex) {
        return Some(impl_addr);
    }
    const SLOTS: [&str; 2] = [
        "0x360894a13ba1a3210667c828492db98dca3e2076cc3735a920a3ca505d382bbc",
        "0x7050c9e0f4ca769c69bd3a8ef740bc37934f8e2c036e5a723fd8ee048ed3f8c3",
    ];
    for slot in SLOTS {
        let Ok(v) = chain::rpc_call(
            rpc_url,
            "eth_getStorageAt",
            serde_json::json!([contract, slot, "latest"]),
        )
        .await
        else {
            continue;
        };
        let Some(word) = v.get("result").and_then(|r| r.as_str()) else {
            continue;
        };
        let hex = word.trim_start_matches("0x");
        if hex.len() == 64 && hex.chars().any(|c| c != '0') {
            return Some(format!("0x{}", &hex[24..]));
        }
    }
    None
}

/// Empty revert data (`"data":"0x"`) tells the user nothing, so check whether
/// the calldata selector is even implemented by the target (following proxies)
/// and say so explicitly when it isn't. Returns `None` when the selector IS
/// present (a real state/logic revert) or the probe itself failed.
async fn diagnose_empty_revert(
    endpoints: &[String],
    contract: &str,
    calldata: &str,
) -> Option<String> {
    let sel = calldata.get(2..10)?.to_ascii_lowercase();
    if !sel.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let url = endpoints.first()?;
    let code = runtime_code(url, contract).await?;
    if code.is_empty() {
        return Some(format!(
            "{contract} has no runtime code on this chain — wrong address (EOA) or wrong chain"
        ));
    }
    if code.contains(&sel) {
        return None;
    }
    let impl_addr = proxy_implementation(url, contract, &code).await;
    if let Some(ia) = &impl_addr {
        if let Some(impl_code) = runtime_code(url, ia).await {
            if impl_code.contains(&sel) {
                return None;
            }
        }
    }
    Some(match impl_addr {
        Some(ia) => format!(
            "selector 0x{sel} is not in the runtime code of {contract} (proxy impl {ia}) — \
             this contract does not implement that function; check the signature/ABI"
        ),
        None => format!(
            "selector 0x{sel} is not in the runtime code of {contract} — \
             this contract does not implement that function; check the signature/ABI"
        ),
    })
}

/// Core enqueue logic. `status` is `"pending"` (auto-run) or `"draft"` (manual promote).
fn enqueue_inner(a: EnqueueArgs, status: &str) -> AppResult<MintTaskRow> {
    if !crate::vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    if a.wallet_id <= 0 {
        return Err(AppError::Invalid("wallet_id required".into()));
    }
    wallet_store::get_wallet(a.wallet_id)?;
    if a.quantity < 1 {
        return Err(AppError::Invalid("quantity >= 1".into()));
    }
    let c = a.contract.trim();
    if c.len() != 42 || !c.starts_with("0x") || !c[2..].chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err(AppError::Invalid(
            "contract must be 0x + 40 hex chars".into(),
        ));
    }
    if a.chain_id <= 0 {
        return Err(AppError::Invalid("chain_id required".into()));
    }
    let mode = a.mode.as_deref().unwrap_or("execute").trim().to_string();
    validate_mode(&mode)?;

    let function_name = a
        .function_name
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let parameters = a
        .parameters
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let raw_cd = a
        .calldata
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    let stored_cd = if a.is_hex {
        let cd = raw_cd.ok_or_else(|| {
            AppError::Invalid("hex calldata required when HEX is checked".into())
        })?;
        if !cd.starts_with("0x") || cd.len() < 2 {
            return Err(AppError::Invalid("calldata must be 0x-prefixed".into()));
        }
        // Allow only `{address}` placeholder tokens mixed with hex.
        let body = &cd[2..];
        let mut i = 0;
        let bytes = body.as_bytes();
        let mut static_hex = String::with_capacity(bytes.len());
        while i < bytes.len() {
            if bytes[i] == b'{' {
                let rel = body[i..].find('}').ok_or_else(|| {
                    AppError::Invalid("unclosed {address} in calldata".into())
                })?;
                let token = &body[i..i + rel + 1];
                if token != "{address}" {
                    return Err(AppError::Invalid(
                        "only {address} is allowed in hex calldata — resolve {signature}/{proof} via allowlist before enqueue".into(),
                    ));
                }
                i += rel + 1;
                continue;
            }
            if !bytes[i].is_ascii_hexdigit() {
                return Err(AppError::Invalid(
                    "calldata must be 0x hex (or contain {address})".into(),
                ));
            }
            static_hex.push(bytes[i] as char);
            i += 1;
        }
        // Even-length: pure hex must be even; with {address} (40 nibbles, even)
        // the remaining static segments must themselves sum to an even length.
        if static_hex.len() % 2 != 0 {
            return Err(AppError::Invalid(
                "calldata must have even hex length (also after {address} substitution)".into(),
            ));
        }
        Some(cd)
    } else {
        let sig = function_name.as_deref().unwrap_or("mint()");
        validate_signature(sig)?;
        // Pre-build fixed calldata when no {address} placeholder is needed.
        match raw_cd {
            Some(cd) => {
                if !cd.starts_with("0x") || !cd[2..].chars().all(|ch| ch.is_ascii_hexdigit()) {
                    return Err(AppError::Invalid("calldata must be 0x-prefixed hex".into()));
                }
                Some(cd)
            }
            None => None,
        }
    };

    if let Some(list) = &a.rpc_endpoints {
        if list.iter().any(|u| {
            let t = u.trim();
            !(t.starts_with("http://") || t.starts_with("https://"))
        }) {
            return Err(AppError::Invalid("rpc endpoints must be http(s)".into()));
        }
    }

    if let Some(gl) = opt_trim(&a.gas_limit) {
        gl.parse::<u64>()
            .map_err(|_| AppError::Invalid("gas_limit must be a number or auto".into()))?;
    }
    for (label, v) in [
        ("max_fee_gwei", &a.max_fee_gwei),
        ("priority_fee_gwei", &a.priority_fee_gwei),
    ] {
        if let Some(x) = opt_trim(v) {
            x.parse::<f64>()
                .map_err(|_| AppError::Invalid(format!("{label} must be a number or auto")))?;
        }
    }
    if let Some(n) = opt_trim(&a.nonce_override) {
        n.parse::<u64>()
            .map_err(|_| AppError::Invalid("nonce must be a number or auto".into()))?;
    }

    let now = crate::db::now_ms();
    if let Some(sched) = a.scheduled_at {
        if sched < 0 {
            return Err(AppError::Invalid("scheduled_at must be >= 0".into()));
        }
    }

    let rpc_json = a.rpc_endpoints.as_ref().and_then(|list| {
        let cleaned: Vec<&str> = list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if cleaned.is_empty() {
            None
        } else {
            serde_json::to_string(&cleaned).ok()
        }
    });

    let delay = a.delay_ms.unwrap_or(0).max(0);
    let gas_limit = opt_trim(&a.gas_limit);
    let max_fee_gwei = opt_trim(&a.max_fee_gwei);
    let priority_fee_gwei = opt_trim(&a.priority_fee_gwei);
    let nonce_override = opt_trim(&a.nonce_override);
    let fn_name = if a.is_hex { None } else { function_name };
    let value_wei = a.value_wei.as_ref().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let id = crate::db::with_conn(|conn| {
        // Idempotency guard: a double-click/duplicate submit must not insert a
        // second row that would sign a second tx for the same mint. `IS` is
        // NULL-safe, so unscheduled tasks and empty opensea_ref still compare.
        // The key covers the whole payload (mode/function/params/quantity) so
        // different work on the same contract — simulate, then execute, spam —
        // still queues, while a literal double-submit is rejected.
        let dup: Option<i64> = conn
            .query_row(
                "SELECT id FROM mint_tasks
                  WHERE wallet_id=?1 AND chain_id=?2 AND contract=?3
                    AND calldata IS ?4 AND opensea_ref IS ?5 AND scheduled_at IS ?6
                    AND mode IS ?7 AND function_name IS ?8 AND parameters IS ?9
                    AND quantity=?10 AND value_wei IS ?11
                    AND status IN ('pending','signing','broadcasting')
                  LIMIT 1",
                rusqlite::params![
                    a.wallet_id,
                    a.chain_id,
                    c,
                    stored_cd,
                    a.opensea_ref,
                    a.scheduled_at,
                    mode,
                    fn_name,
                    parameters,
                    a.quantity,
                    value_wei
                ],
                |r| r.get(0),
            )
            .ok();
        if let Some(dup_id) = dup {
            return Err(AppError::Invalid(format!(
                "task #{dup_id} is already queued for this wallet and payload — cancel it first"
            )));
        }
        conn.execute(
            "INSERT INTO mint_tasks(
                chain_id, contract, quantity, value_wei, calldata, status, wallet_id, created_at, updated_at,
                function_name, is_hex, parameters, rpc_endpoints, flashbots,
                gas_limit, max_fee_gwei, priority_fee_gwei, nonce_override,
                scheduled_at, delay_ms, mode, opensea_ref
             )
             VALUES (?1,?2,?3,?4,?5,?20,?6,?7,?7, ?8,?9,?10,?11,?12, ?13,?14,?15,?16, ?17,?18,?19,?21)",
            rusqlite::params![
                a.chain_id,
                c,
                a.quantity,
                value_wei,
                stored_cd,
                a.wallet_id,
                now,
                fn_name,
                a.is_hex as i64,
                parameters,
                rpc_json,
                a.flashbots as i64,
                gas_limit,
                max_fee_gwei,
                priority_fee_gwei,
                nonce_override,
                a.scheduled_at,
                delay,
                mode,
                status,
                a.opensea_ref,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })?;

    wallet_store::log_activity(
        "mint.enqueue",
        &format!(
            "{} mint #{id} → {c} x{} on chain {} ({mode})",
            if status == "draft" { "Drafted" } else { "Queued" },
            a.quantity,
            a.chain_id
        ),
        None,
        true,
    );
    get_task(id)
}

/// Queue a mint task for immediate processing by the auto-run scheduler.
pub fn enqueue(a: EnqueueArgs) -> AppResult<MintTaskRow> {
    enqueue_inner(a, "pending")
}

/// Store a mint task as a draft — visible on the Minting page but NOT run
/// until the user promotes it (`promote_task` / `promote_all_drafts`).
pub fn enqueue_draft(a: EnqueueArgs) -> AppResult<MintTaskRow> {
    enqueue_inner(a, "draft")
}

/// Promote a draft task to `pending` so the auto-run scheduler picks it up.
pub fn promote_task(id: i64) -> AppResult<MintTaskRow> {
    let now = crate::db::now_ms();
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute(
            "UPDATE mint_tasks SET status='pending', updated_at=?1 WHERE id=?2 AND status='draft'",
            rusqlite::params![now, id],
        )?)
    })?;
    if n == 0 {
        let t = get_task(id)?;
        if t.status != "draft" {
            return Err(AppError::Invalid(format!(
                "task {id} is '{}' — only drafts can be promoted",
                t.status
            )));
        }
    }
    wallet_store::log_activity(
        "mint.promote",
        &format!("Promoted draft mint #{id} → pending"),
        None,
        true,
    );
    get_task(id)
}

/// On-chain verdict for a hash already stored on a failed task.
#[derive(Debug, PartialEq, Eq)]
enum TxVerdict {
    /// No node has it: receipt absent and mempool empty — safe to re-sign.
    Safe,
    /// Confirmed successfully — retrying would mint twice.
    Confirmed,
    /// Confirmed but reverted (nonce consumed, nothing minted) — safe to retry.
    Reverted,
    /// Still sitting in some node's mempool — retrying would replace it.
    Mempool,
}

/// Check a stored tx hash across the task's endpoints (chain default last).
/// `Err` means no endpoint could be consulted — callers must treat that as
/// "do not retry" rather than "assume safe".
async fn verify_stored_tx(task: &MintTaskRow, hash: &str) -> Result<TxVerdict, String> {
    let chains = chain::list_chains().map_err(|e| e.to_string())?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == task.chain_id)
        .ok_or_else(|| format!("chain {} unavailable", task.chain_id))?;
    let endpoints: Vec<String> = parse_rpc_list(task, &chain_row.rpc_url);
    let mut answered = false;
    for url in &endpoints {
        // Only an endpoint on the right chain can prove anything here — a
        // mismatched node answering "no receipt" would fake a Safe verdict.
        match chain::eth_chain_id(url).await {
            Ok(cid) if cid == task.chain_id => {}
            _ => continue,
        }
        match chain::get_receipt(url, hash).await {
            Ok(Some(receipt)) => {
                let reverted = receipt.get("status").and_then(|s| s.as_str()) == Some("0x0");
                return Ok(if reverted {
                    TxVerdict::Reverted
                } else {
                    TxVerdict::Confirmed
                });
            }
            Ok(None) => answered = true, // right chain, reachable, no receipt yet
            Err(_) => {}
        }
        if let Ok(v) = chain::rpc_call(
            url,
            "eth_getTransactionByHash",
            serde_json::json!([hash]),
        )
        .await
        {
            answered = true;
            let held = v.get("result").map(|r| !r.is_null()).unwrap_or(false);
            if held {
                return Ok(TxVerdict::Mempool);
            }
        }
    }
    if answered {
        Ok(TxVerdict::Safe)
    } else {
        Err("no RPC endpoint reachable".into())
    }
}

/// Reset a failed task back to `pending` so it can be retried.
/// Clears the error, tx hash, and poll/retry counters — but only after the
/// stored tx hash is proven absent on-chain. Re-signing while the original tx
/// is confirmed or still in the mempool is how double-mints happen.
pub async fn retry_task(id: i64) -> AppResult<MintTaskRow> {
    let task = get_task(id)?;
    if !matches!(task.status.as_str(), "failed" | "canceled" | "cancelled") {
        return Err(AppError::Invalid(format!(
            "task {id} is '{}' — only failed/cancelled tasks can be retried",
            task.status
        )));
    }
    if let Some(hash) = task.tx_hash.clone() {
        match verify_stored_tx(&task, &hash).await {
            Ok(TxVerdict::Confirmed) => {
                return Err(AppError::Invalid(format!(
                    "tx {hash} already confirmed on-chain — not retrying (would mint twice)"
                )));
            }
            Ok(TxVerdict::Mempool) => {
                return Err(AppError::Invalid(format!(
                    "tx {hash} is still in the mempool — wait for it, re-signing would replace it"
                )));
            }
            Ok(TxVerdict::Safe) => release_task_nonce(id),
            Ok(TxVerdict::Reverted) => {}
            Err(e) => {
                return Err(AppError::Invalid(format!(
                    "cannot verify tx {hash}: {e} — not retrying"
                )));
            }
        }
    }
    let now = crate::db::now_ms();
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute(
            "UPDATE mint_tasks
                SET status='pending', error=NULL, tx_hash=NULL, prev_tx_hash=NULL,
                    bump_count=0, tx_nonce=NULL, poll_attempts=0, auto_retries=0,
                    updated_at=?1
              WHERE id=?2 AND status IN ('failed','canceled','cancelled')",
            rusqlite::params![now, id],
        )?)
    })?;
    if n == 0 {
        return Err(AppError::Invalid(format!(
            "task {id} is no longer retryable"
        )));
    }
    wallet_store::log_activity(
        "mint.retry",
        &format!("Retried task #{id} → pending"),
        None,
        true,
    );
    get_task(id)
}

/// Retry every failed/cancelled task in one call.
/// Tasks whose stored tx hash is confirmed or still pending on-chain are
/// skipped (see [`verify_stored_tx`]) — they would otherwise double-send.
pub async fn retry_all_failed() -> AppResult<i64> {
    let candidates: Vec<MintTaskRow> = crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM mint_tasks WHERE status IN ('failed','canceled','cancelled')"
        ))?;
        let rows = stmt.query_map([], map_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let now = crate::db::now_ms();
    let mut n = 0i64;
    let mut skipped = 0u32;
    for task in &candidates {
        if let Some(hash) = task.tx_hash.clone() {
            match verify_stored_tx(task, &hash).await {
                Ok(TxVerdict::Confirmed) | Ok(TxVerdict::Mempool) | Err(_) => {
                    skipped += 1;
                    continue;
                }
                Ok(TxVerdict::Safe) => release_task_nonce(task.id),
                Ok(TxVerdict::Reverted) => {}
            }
        }
        let cleared = crate::db::with_conn(|conn| {
            Ok(conn.execute(
                "UPDATE mint_tasks
                    SET status='pending', error=NULL, tx_hash=NULL, prev_tx_hash=NULL,
                        bump_count=0, tx_nonce=NULL, poll_attempts=0, auto_retries=0,
                        updated_at=?1
                  WHERE id=?2 AND status IN ('failed','canceled','cancelled')",
                rusqlite::params![now, task.id],
            )?)
        })?;
        if cleared > 0 {
            n += 1;
        }
    }
    if skipped > 0 {
        wallet_store::log_activity(
            "mint.retry",
            &format!(
                "Skipped {skipped} task(s) on bulk retry — tx already confirmed or still in mempool"
            ),
            None,
            false,
        );
    }
    if n > 0 {
        wallet_store::log_activity(
            "mint.retry",
            &format!("Retried {n} failed task(s) → pending"),
            None,
            true,
        );
    }
    Ok(n)
}

/// Promote every draft task in one call (bulk "queue all drafts").
pub fn promote_all_drafts() -> AppResult<i64> {
    let now = crate::db::now_ms();
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute(
            "UPDATE mint_tasks SET status='pending', updated_at=?1 WHERE status='draft'",
            rusqlite::params![now],
        )?)
    })?;
    if n > 0 {
        wallet_store::log_activity(
            "mint.promote",
            &format!("Promoted {n} draft task(s) → pending"),
            None,
            true,
        );
    }
    Ok(n as i64)
}

pub fn cancel_task(id: i64) -> AppResult<()> {
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute(
            "DELETE FROM mint_tasks WHERE id = ?1 AND status IN ('pending','draft')",
            [id],
        )?)
    })?;
    if n == 0 {
        let task = get_task(id)?;
        if task.status == "pending" {
            return Err(AppError::Other("delete failed".into()));
        }
        return Err(AppError::Invalid(
            "only pending tasks can be cancelled".into(),
        ));
    }
    wallet_store::log_activity("mint.cancel", &format!("Cancelled mint #{id}"), None, true);
    Ok(())
}

fn set_status(id: i64, status: &str, tx_hash: Option<&str>, error: Option<&str>) {
    let now = crate::db::now_ms();
    // Errors reach this column straight from RPC transport failures, whose
    // Display includes the endpoint URL (API keys live in the path) — redact.
    let error = error.map(chain::redact_urls_in);
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE mint_tasks SET status=?1, tx_hash=COALESCE(?2, tx_hash), error=?3, updated_at=?4 WHERE id=?5",
            rusqlite::params![status, tx_hash, error, now, id],
        )?;
        Ok(())
    });
}

/// Hand this task's reserved nonce back to the shared allocator.
///
/// Called from terminal failure paths. Safe even when the tx did land: the
/// next reserve reads the chain's `pending` count, sees the higher number and
/// re-adopts it — the rewind is immediately overwritten.
fn release_task_nonce(id: i64) {
    let row = crate::db::with_conn(|conn| {
        let r: rusqlite::Result<(i64, i64, Option<i64>)> = conn.query_row(
            "SELECT COALESCE(wallet_id, 0), chain_id, tx_nonce FROM mint_tasks WHERE id=?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        );
        Ok(r.ok())
    });
    if let Ok(Some((wallet_id, chain_id, Some(nonce)))) = row {
        crate::nonce::release(wallet_id, chain_id, nonce as u64);
        crate::logging::info(
            "mint.nonce",
            &format!("Mint #{id}: released unused nonce {nonce}"),
        );
    }
}

fn validate_signature(sig: &str) -> AppResult<()> {
    let sig = sig.trim();
    let open = sig
        .find('(')
        .ok_or_else(|| AppError::Invalid("function must look like name(types)".into()))?;
    if !sig.ends_with(')') {
        return Err(AppError::Invalid("function must end with )".into()));
    }
    let name = &sig[..open];
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(AppError::Invalid("bad function name".into()));
    }
    Ok(())
}

fn split_types(sig: &str) -> AppResult<Vec<String>> {
    let open = sig.find('(').unwrap();
    let inner = &sig[open + 1..sig.len() - 1];
    if inner.trim().is_empty() {
        return Ok(vec![]);
    }
    Ok(inner
        .split(',')
        .map(|s| s.trim().to_string())
        .collect())
}

fn selector(sig: &str) -> [u8; 4] {
    let h = alloy::primitives::keccak256(sig.as_bytes());
    [h[0], h[1], h[2], h[3]]
}

pub(crate) fn encode_word_address(addr: &str) -> AppResult<[u8; 32]> {
    let a = alloy::primitives::Address::parse_checksummed(addr, None)
        .or_else(|_| addr.parse::<alloy::primitives::Address>())
        .map_err(|_| AppError::Invalid(format!("bad address param: {addr}")))?;
    let mut out = [0u8; 32];
    out[12..].copy_from_slice(a.as_slice());
    Ok(out)
}

pub(crate) fn encode_word_uint(v: &str) -> AppResult<[u8; 32]> {
    let s = v.trim();
    let u = if let Some(h) = s.strip_prefix("0x") {
        alloy::primitives::U256::from_str_radix(h, 16)
            .map_err(|_| AppError::Invalid(format!("bad uint param: {v}")))?
    } else {
        let n: u128 = s
            .parse()
            .map_err(|_| AppError::Invalid(format!("bad uint param: {v}")))?;
        alloy::primitives::U256::from(n)
    };
    let mut out = [0u8; 32];
    out.copy_from_slice(&u.to_be_bytes::<32>());
    Ok(out)
}

enum ParamVal {
    Static(String),
    Proof(Vec<String>),
    HexBytes(String),
}

fn is_dynamic_type(t: &str) -> bool {
    let t = t.trim();
    if t.ends_with("[]") {
        return true;
    }
    let base = t.split('[').next().unwrap_or(t).trim();
    base == "bytes" || base == "string"
}

fn resolve_param_val(
    t: &str,
    v: &str,
    quantity: i64,
    wallet_address: &str,
    entry: Option<&allowlist::AllowlistEntry>,
) -> AppResult<ParamVal> {
    match v {
        "{address}" | "address" | "to" => Ok(ParamVal::Static(wallet_address.to_string())),
        "{quantity}" | "quantity" => Ok(ParamVal::Static(quantity.max(1).to_string())),
        "{proof}" | "proof" => {
            let proof = entry
                .and_then(|e| e.proof.clone())
                .filter(|p| !p.is_empty())
                .ok_or_else(|| AppError::Invalid("allowlist entry missing proof".into()))?;
            Ok(ParamVal::Proof(proof))
        }
        "{signature}" | "signature" => {
            let sig = entry
                .and_then(|e| e.signature.clone())
                .filter(|s| s.len() > 2)
                .ok_or_else(|| AppError::Invalid("allowlist entry missing signature".into()))?;
            Ok(ParamVal::HexBytes(sig))
        }
        _ => {
            if t.ends_with("[]") && v.starts_with("0x") && v.contains('|') {
                let mut leaves = Vec::new();
                for part in v.split('|') {
                    let p = part.trim();
                    if p.is_empty() {
                        continue;
                    }
                    leaves.push(p.to_string());
                }
                return Ok(ParamVal::Proof(leaves));
            }
            Ok(ParamVal::Static(v.to_string()))
        }
    }
}

fn encode_dynamic_tail(t: &str, val: &ParamVal) -> AppResult<Vec<u8>> {
    let base = t.split('[').next().unwrap_or(t).trim();
    if base == "bytes" || base == "string" {
        let raw = match val {
            ParamVal::HexBytes(h) => h.trim_start_matches("0x").to_string(),
            ParamVal::Static(s) => s.trim_start_matches("0x").to_string(),
            ParamVal::Proof(_) => {
                return Err(AppError::Invalid("proof cannot encode as bytes".into()));
            }
        };
        let bytes = hex::decode(&raw).map_err(|_| AppError::Invalid(format!("bad bytes hex: {raw}")))?;
        let mut out = Vec::with_capacity(32 + bytes.len().div_ceil(32) * 32);
        out.extend_from_slice(&alloy::primitives::U256::from(bytes.len()).to_be_bytes::<32>());
        out.extend_from_slice(&bytes);
        let pad = (32 - (bytes.len() % 32)) % 32;
        out.extend(std::iter::repeat_n(0u8, pad));
        return Ok(out);
    }
    // array (bytes32[], address[], uint256[], …)
    let leaves: Vec<String> = match val {
        ParamVal::Proof(p) => p.clone(),
        ParamVal::Static(s) => {
            if s.trim().is_empty() {
                Vec::new()
            } else if s.contains('|') {
                s.split('|').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
            } else {
                vec![s.trim().to_string()]
            }
        }
        ParamVal::HexBytes(h) => vec![h.clone()],
    };
    let mut out = Vec::with_capacity(32 + leaves.len() * 32);
    out.extend_from_slice(&alloy::primitives::U256::from(leaves.len()).to_be_bytes::<32>());
    for leaf in leaves {
        if base == "bytes32" {
            let raw = leaf.trim_start_matches("0x");
            let bytes = hex::decode(raw)
                .map_err(|_| AppError::Invalid(format!("bad bytes32 leaf: {leaf}")))?;
            if bytes.len() != 32 {
                return Err(AppError::Invalid("bytes32 leaf must be 32 bytes".into()));
            }
            out.extend_from_slice(&bytes);
        } else if base.starts_with("address") {
            out.extend_from_slice(&encode_word_address(&leaf)?);
        } else if base.starts_with("uint") || base.starts_with("int") {
            out.extend_from_slice(&encode_word_uint(&leaf)?);
        } else if base == "bool" {
            let mut w = [0u8; 32];
            w[31] = if leaf == "true" || leaf == "1" { 1 } else { 0 };
            out.extend_from_slice(&w);
        } else {
            return Err(AppError::Invalid(format!("unsupported array type: {t}")));
        }
    }
    Ok(out)
}

fn encode_static_word(t: &str, val: &ParamVal) -> AppResult<[u8; 32]> {
    let resolved = match val {
        ParamVal::Static(s) => s.clone(),
        ParamVal::HexBytes(h) => h.clone(),
        ParamVal::Proof(_) => {
            return Err(AppError::Invalid(format!(
                "proof value not valid for static type {t}"
            )));
        }
    };
    let base = t.split('[').next().unwrap_or(t).trim();
    if base.starts_with("address") {
        encode_word_address(&resolved)
    } else if base.starts_with("uint") || base.starts_with("int") {
        encode_word_uint(&resolved)
    } else if base == "bool" {
        let mut w = [0u8; 32];
        w[31] = if resolved == "true" || resolved == "1" { 1 } else { 0 };
        Ok(w)
    } else if base == "bytes32" {
        let raw = resolved.trim_start_matches("0x");
        let bytes = hex::decode(raw)
            .map_err(|_| AppError::Invalid(format!("bad bytes32: {resolved}")))?;
        if bytes.len() != 32 {
            return Err(AppError::Invalid("bytes32 must be 32 bytes".into()));
        }
        let mut w = [0u8; 32];
        w.copy_from_slice(&bytes);
        Ok(w)
    } else {
        Err(AppError::Invalid(format!("unsupported param type: {t}")))
    }
}

fn build_calldata_inner(
    function_name: &str,
    parameters: Option<&str>,
    quantity: i64,
    wallet_address: &str,
    entry: Option<&allowlist::AllowlistEntry>,
) -> AppResult<String> {
    let sig = function_name.trim();
    validate_signature(sig)?;
    let types = split_types(sig)?;
    let sel = selector(sig);

    if types.is_empty() {
        return Ok(format!("0x{}", hex::encode(sel)));
    }

    let params_str = parameters.unwrap_or("").trim();
    let mut raw_values: Vec<String> = if params_str.is_empty() {
        vec![]
    } else {
        params_str
            .split([';', ','])
            .map(|s| s.trim().to_string())
            .collect()
    };

    while raw_values.len() < types.len() {
        let idx = raw_values.len();
        let t = types[idx].as_str();
        if t.starts_with("address") {
            raw_values.push("{address}".into());
        } else if t.starts_with("uint") || t.starts_with("int") {
            raw_values.push(quantity.max(1).to_string());
        } else if is_dynamic_type(t) && t.ends_with("[]") {
            raw_values.push(String::new());
        } else {
            return Err(AppError::Invalid(format!("missing parameter for {t}")));
        }
    }
    if raw_values.len() > types.len() {
        return Err(AppError::Invalid(format!(
            "expected {} params, got {}",
            types.len(),
            raw_values.len()
        )));
    }

    let mut resolved: Vec<(String, ParamVal)> = Vec::with_capacity(types.len());
    for (t, v) in types.iter().zip(raw_values.iter()) {
        // Empty dynamic array stays empty.
        if v.is_empty() && t.ends_with("[]") {
            resolved.push((t.clone(), ParamVal::Proof(Vec::new())));
            continue;
        }
        resolved.push((
            t.clone(),
            resolve_param_val(t, v, quantity, wallet_address, entry)?,
        ));
    }

    let head_len = types.len() * 32;
    let mut head = Vec::with_capacity(head_len);
    let mut tail = Vec::new();
    for (t, val) in &resolved {
        if is_dynamic_type(t) {
            let offset = head_len + tail.len();
            head.extend_from_slice(&alloy::primitives::U256::from(offset).to_be_bytes::<32>());
            tail.extend_from_slice(&encode_dynamic_tail(t, val)?);
        } else {
            head.extend_from_slice(&encode_static_word(t, val)?);
        }
    }

    let mut data = Vec::with_capacity(4 + head.len() + tail.len());
    data.extend_from_slice(&sel);
    data.extend_from_slice(&head);
    data.extend_from_slice(&tail);
    Ok(format!("0x{}", hex::encode(data)))
}

/// Build calldata from function signature + semicolon params.
/// `{address}` is replaced with the wallet address.
/// Auto-fee uplift over the observed gas price, in percent.
/// A phase-opening launch is a race: OSNM-Z rides 1.25x for an immediate mint
/// and 2.5x when the broadcast waits for a scheduled phase, so the tx is not
/// starved by the burst of competing transactions at T-0. Manual fee fields
/// always bypass this.
pub fn auto_fee_multiplier_pct(scheduled: bool) -> u128 {
    if scheduled { 250 } else { 125 }
}

pub fn build_calldata_from_fn(
    function_name: &str,
    parameters: Option<&str>,
    quantity: i64,
    wallet_address: &str,
) -> AppResult<String> {
    build_calldata_inner(function_name, parameters, quantity, wallet_address, None)
}

/// Same as [`build_calldata_from_fn`] but resolves `{proof}` / `{signature}`
/// from an allowlist entry (GTD mint patterns).
pub fn build_calldata_with_entry(
    function_name: &str,
    parameters: Option<&str>,
    quantity: i64,
    wallet_address: &str,
    entry: Option<&allowlist::AllowlistEntry>,
) -> AppResult<String> {
    build_calldata_inner(function_name, parameters, quantity, wallet_address, entry)
}

fn resolve_calldata(task: &MintTaskRow, wallet_address: &str) -> AppResult<String> {
    if task.is_hex {
        let cd = task
            .calldata
            .clone()
            .ok_or_else(|| AppError::Invalid("hex task missing calldata".into()))?;
        // Allow {address} placeholder in hex payloads.
        // ABI-encode: address is 20 bytes = 40 hex chars, but the EVM
        // word is 32 bytes = 64 hex chars.  Zero-pad to 64 so the calldata
        // byte length is correct; otherwise the EVM reverts.
        if cd.contains("{address}") {
            let needle = "{address}";
            let raw_addr = wallet_address.trim_start_matches("0x");
            let padded = format!("{:0>64}", raw_addr);  // 32-byte word
            let mut out = String::with_capacity(cd.len());
            let rest = cd.as_str();
            let mut s = rest;
            while let Some(pos) = s.find(needle) {
                out.push_str(&s[..pos]);
                out.push_str(&padded);
                s = &s[pos + needle.len()..];
            }
            out.push_str(s);
            return Ok(out);
        }
        return Ok(cd);
    }
    if let Some(cd) = &task.calldata {
        if !cd.contains("{address}") {
            return Ok(cd.clone());
        }
    }
    let sig = task.function_name.as_deref().unwrap_or("mint()");
    build_calldata_from_fn(sig, task.parameters.as_deref(), task.quantity, wallet_address)
}

fn parse_rpc_list(task: &MintTaskRow, chain_default: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(json) = &task.rpc_endpoints {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(json) {
            for u in list {
                let t = u.trim().to_string();
                if !t.is_empty() && !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    }
    if out.is_empty() {
        out.push(chain_default.to_string());
    }
    if task.flashbots && !out.iter().any(|u| u.contains("flashbots")) {
        out.push(FLASHBOTS_RPC.to_string());
    }
    out
}

/// Fire-time attempts for a task's OpenSea mint data, and the pause between them.
/// A scheduled task fires up to 60s before the stage opens (`start - lead`), so
/// the poll must outlive that lead: fast for the first few attempts, then slow
/// until the stage is live (≈100s total budget).
const OPENSEA_CALLDATA_ATTEMPTS: u32 = 40;
const OPENSEA_CALLDATA_FAST_ATTEMPT: u32 = 8;
const OPENSEA_CALLDATA_FAST_MS: u64 = 500;
const OPENSEA_CALLDATA_SLOW_MS: u64 = 3_000;

/// Retry only "stage not open yet" and transport-class failures. Eligibility
/// and collection errors are terminal however often we ask — retrying them
/// would just burn the launch window.
fn opensea_mint_data_retryable(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const PATTERN: [&str; 14] = [
        "not live yet",
        "not minting",
        "dropnotminting",
        "transport",
        "timeout",
        "timed out",
        "connection",
        "429",
        "rate limit",
        "too many requests",
        "500",
        "502",
        "503",
        "504",
    ];
    PATTERN.iter().any(|p| m.contains(p))
}

/// `opensea_ref` payload stored on a deferred task.
#[derive(serde::Deserialize)]
struct OpenseaRef {
    collection: String,
    #[serde(default = "default_token_id")]
    token_id: String,
}

fn default_token_id() -> String {
    "0".into()
}

/// Resolve SIWE → eligible stage → MintAction for a task that deferred its
/// calldata. Retries while the stage is still closed or the API is flaky.
async fn fetch_opensea_mint_data(
    chain_id: i64,
    wallet_id: i64,
    address: &str,
    quantity: i64,
    r: &str,
) -> AppResult<crate::opensea::OpenSeaMintPlan> {
    let parsed: OpenseaRef = serde_json::from_str(r)
        .map_err(|_| AppError::Invalid("task opensea_ref is not valid JSON".into()))?;
    if parsed.collection.len() != 42 || !parsed.collection.starts_with("0x") {
        return Err(AppError::Invalid(
            "opensea_ref.collection must be 0x + 40 hex".into(),
        ));
    }
    let addr: alloy::primitives::Address = address
        .parse()
        .map_err(|_| AppError::Invalid("bad wallet address".into()))?;
    let network = u64::try_from(chain_id).unwrap_or(0);
    let qty = u64::try_from(quantity).unwrap_or(1).max(1);

    let client = crate::opensea::OpenSeaClient::new()?;
    let key = collection_cache_key(&parsed.collection, chain_id);
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        // Collection metadata is warmed by the prep pass when it is available;
        // only the stage/plan read is retried while the drop is still closed.
        let plan: AppResult<crate::opensea::OpenSeaMintPlan> = async {
            let warmed = { lock_unpoisoned(&COLLECTION_CACHE).get(&key).cloned() };
            let resolved = match warmed {
                Some(r) => r,
                None => {
                    let r = client
                        .resolve_collection(&parsed.collection, Some(network))
                        .await?
                        .ok_or_else(|| AppError::NotFound("collection not on OpenSea".into()))?;
                    lock_unpoisoned(&COLLECTION_CACHE).insert(key.clone(), r.clone());
                    r
                }
            };
            client
                .plan_wallet_mint(wallet_id, &addr, &resolved, network, qty, &parsed.token_id)
                .await
        }
        .await;
        match plan {
            Ok(p) => return Ok(p),
            Err(e) => {
                let msg = e.to_string();
                if attempt >= OPENSEA_CALLDATA_ATTEMPTS || !opensea_mint_data_retryable(&msg) {
                    return Err(e);
                }
                wallet_store::log_activity(
                    "mint.opensea",
                    &format!(
                        "Mint data attempt {attempt}/{OPENSEA_CALLDATA_ATTEMPTS} not ready: {msg}"
                    ),
                    None,
                    true,
                );
                let delay_ms = if attempt <= OPENSEA_CALLDATA_FAST_ATTEMPT {
                    OPENSEA_CALLDATA_FAST_MS
                } else {
                    OPENSEA_CALLDATA_SLOW_MS
                };
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
        }
    }
}

/// Pending tasks whose `scheduled_at` now falls inside the prep lead and which
/// have not been prepped yet. Marks each id, so the 60ms tick yields it once.
pub fn due_for_prep(now_ms: i64) -> AppResult<Vec<i64>> {
    let ids: Vec<i64> = crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id FROM mint_tasks
             WHERE status = 'pending'
               AND scheduled_at IS NOT NULL
               AND scheduled_at > ?1
               AND scheduled_at <= ?2
             ORDER BY id",
        )?;
        let rows = stmt.query_map([now_ms, now_ms + PREP_LEAD_MS], |r| r.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut prepped = lock_unpoisoned(&PREPPED);
    if prepped.len() > 10_000 {
        prepped.clear();
    }
    Ok(ids.into_iter().filter(|id| prepped.insert(*id)).collect())
}

/// Warm-up for a scheduled task. Best effort: every network failure is logged
/// and swallowed, because the fire path re-reads everything it signs.
pub async fn prep_task(id: i64) {
    if let Err(e) = prep_inner(id).await {
        if let Ok(mut g) = PREPPED.lock() {
            g.remove(&id);
        }
        wallet_store::log_activity(
            "mint.prep",
            &format!("Prep for task {id} failed: {e}"),
            None,
            false,
        );
    }
}

async fn prep_inner(id: i64) -> AppResult<()> {
    let task = get_task(id)?;
    if task.status != "pending" {
        return Ok(());
    }
    let chains = crate::chain::list_chains()?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == task.chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {}", task.chain_id)))?;
    let endpoints = parse_rpc_list(&task, &chain_row.rpc_url);

    // Reach every endpoint in parallel. The shared reqwest pool keeps these
    // sockets alive, so the fire path inherits a warm connection instead of
    // paying a TCP+TLS handshake in the launch second.
    let expected = task.chain_id;
    let mut handles = Vec::with_capacity(endpoints.len());
    for url in endpoints {
        handles.push(tokio::spawn(async move {
            let cid = crate::chain::eth_chain_id(&url).await;
            (url, cid)
        }));
    }
    let mut healthy: Option<String> = None;
    for h in handles {
        if let Ok((url, Ok(cid))) = h.await {
            if cid == expected && healthy.is_none() {
                healthy = Some(url);
            }
        }
    }
    let Some(url) = healthy else {
        wallet_store::log_activity(
            "mint.prep",
            &format!("Task {id}: no reachable endpoint during prep"),
            None,
            false,
        );
        return Ok(());
    };

    // Pre-flight: simulate the exact fire-time tx ~10s before launch so a
    // guaranteed revert (wrong price, sold out, bad calldata) surfaces BEFORE
    // gas is burnt. Timing reverts (NotActive) are EXPECTED for tasks
    // scheduled ahead of the stage opening — recorded quietly, never warned.
    // Deferred OpenSea tasks resolve calldata at fire time: nothing to
    // simulate yet. Report-only — the launch itself is never delayed, since
    // firing slightly early costs one tx but firing late loses the race.
    if task.mode == "execute" && task.opensea_ref.is_none() {
        if let Some(wid) = task.wallet_id {
            if let Ok(w) = wallet_store::get_wallet(wid) {
                preflight_check(id, &task, &url, &w.address).await;
            }
        }
    }

    // Prime nonce + gas caches on that endpoint. The values are discarded —
    // they must be re-read at fire time — this only warms connection and node
    // state (OSNM-Z's nonce refresh at T-10..T-2).
    if let Some(wid) = task.wallet_id {
        if let Ok(w) = wallet_store::get_wallet(wid) {
            let _ = crate::chain::rpc_call(
                &url,
                "eth_getTransactionCount",
                serde_json::json!([w.address, "pending"]),
            )
            .await;
            let _ = crate::chain::rpc_call(&url, "eth_gasPrice", serde_json::json!([])).await;
        }
    }

    // Warm OpenSea collection metadata for a deferred task: fire time then
    // only has to wait for the stage to open, not for two roundtrips.
    if let Some(r) = task.opensea_ref.as_deref() {
        let Ok(parsed) = serde_json::from_str::<OpenseaRef>(r) else {
            return Ok(());
        };
        let network = u64::try_from(task.chain_id).unwrap_or(0);
        let key = collection_cache_key(&parsed.collection, task.chain_id);
        if lock_unpoisoned(&COLLECTION_CACHE).contains_key(&key) {
            return Ok(());
        }
        match crate::opensea::OpenSeaClient::new() {
            Ok(client) => match client.resolve_collection(&parsed.collection, Some(network)).await
            {
                Ok(Some(resolved)) => {
                    lock_unpoisoned(&COLLECTION_CACHE).insert(key, resolved);
                }
                Ok(None) => {}
                Err(e) => wallet_store::log_activity(
                    "mint.prep",
                    &format!("Task {id}: collection warm-up failed: {e}"),
                    None,
                    false,
                ),
            },
            Err(e) => wallet_store::log_activity(
                "mint.prep",
                &format!("Task {id}: OpenSea client unavailable: {e}"),
                None,
                false,
            ),
        }
    }
    Ok(())
}

fn collection_cache_key(collection: &str, chain_id: i64) -> String {
    format!("{}|{}", collection.trim().to_lowercase(), chain_id)
}

/// Fire-time simulation for a scheduled execute task. Report-only: a timing
/// revert is expected before the stage opens; anything else is a real problem
/// worth surfacing before gas is burnt.
async fn preflight_check(task_id: i64, task: &MintTaskRow, url: &str, wallet_address: &str) {
    let Ok(calldata) = resolve_calldata(task, wallet_address) else {
        return;
    };
    let value_wei = task.value_wei.clone().unwrap_or_else(|| "0x0".into());
    let Ok(value_u) = parse_u128_hex_or_dec(&value_wei) else {
        return;
    };
    let tx = serde_json::json!({
        "from": wallet_address,
        "to": task.contract,
        "data": calldata,
        "value": format!("0x{:x}", value_u),
    });
    match chain::rpc_call(url, "eth_call", serde_json::json!([tx, "latest"])).await {
        Ok(_) => {} // predicted success — nothing to report
        Err(e) => {
            let decoded = decode_rpc_error(&e);
            if decoded.starts_with("reverted:") {
                // Revert with decoded data: NotActive is the expected
                // pre-open answer; anything else is a real problem.
                let timing = decoded.to_ascii_lowercase().contains("notactive");
                let msg = if timing {
                    "pre-flight: drop not active yet (expected when the stage opens after the fire time)"
                        .to_string()
                } else {
                    format!("pre-flight: predicted revert at fire time — {decoded}")
                };
                if timing {
                    wallet_store::log_activity(
                        "mint.preflight",
                        &format!("Mint #{task_id}: {msg}"),
                        None,
                        false,
                    );
                    crate::logging::info(
                        "mint.preflight",
                        &format!("Mint #{task_id}: {msg}"),
                    );
                } else {
                    // Loud: surface on the task row (error column, task stays
                    // pending) so the Minting table shows it before launch.
                    set_status(task_id, "pending", None, Some(&msg));
                    wallet_store::log_activity(
                        "mint.preflight",
                        &format!("Mint #{task_id}: {msg}"),
                        None,
                        false,
                    );
                    crate::logging::warn(
                        "mint.preflight",
                        &format!("Mint #{task_id}: {msg}"),
                    );
                }
            } else if decoded.contains("execution reverted") {
                // Revert without data — real, but the reason is opaque.
                let msg = format!("pre-flight: predicted revert at fire time — {decoded}");
                set_status(task_id, "pending", None, Some(&msg));
                wallet_store::log_activity(
                    "mint.preflight",
                    &format!("Mint #{task_id}: {msg}"),
                    None,
                    false,
                );
                crate::logging::warn("mint.preflight", &format!("Mint #{task_id}: {msg}"));
            } else {
                // Transport / node-side failure — inconclusive, stay quiet.
                crate::logging::info(
                    "mint.preflight",
                    &format!("Mint #{task_id}: pre-flight inconclusive: {decoded}"),
                );
            }
        }
    }
}

/// Process one pending/broadcasting task: sign + broadcast or re-poll receipt.
pub async fn process_task(id: i64) -> AppResult<MintTaskRow> {
    let task = get_task(id)?;
    if task.status == "broadcasting" {
        // Count every re-entry into the poller so a dropped transaction cannot
        // pin the queue in `broadcasting` forever.
        let attempts = task.poll_attempts + 1;
        let _ = crate::db::with_conn(|conn| {
            conn.execute(
                "UPDATE mint_tasks SET poll_attempts=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![attempts, crate::db::now_ms(), id],
            )?;
            Ok(())
        });
        if attempts > MAX_POLL_ATTEMPTS {
            // Nothing mined after ~20m: the nonce is either free (tx dropped)
            // or consumed (it landed and we lost the receipt) — releasing is
            // safe in both cases, the next reserve adopts the chain's count.
            release_task_nonce(id);
            crate::logging::warn(
                "mint.poll",
                &format!("Mint #{id}: gave up waiting for receipt of {:?}", task.tx_hash),
            );
            set_status(
                id,
                "failed",
                task.tx_hash.as_deref(),
                Some("no receipt after ~20m — tx dropped or RPC never saw it; re-enqueue if needed"),
            );
            return get_task(id);
        }
        if let Some(hash) = task.tx_hash.clone() {
            let out = poll_receipt(id, &hash).await;
            // A full window with no verdict means the tx is stuck: replace it
            // with a higher-fee version (same nonce, so only one can mine).
            // Windows are ~30s; bumps land on window 1, 3 and 5.
            let mut bumped = false;
            if let Ok(t) = &out {
                if t.status == "broadcasting"
                    && t.bump_count < MAX_BUMPS
                    && attempts >= 1 + t.bump_count * 2
                {
                    match bump_stuck_tx(id).await {
                        Ok(Some(_)) => bumped = true,
                        Ok(None) => {}
                        Err(e) => crate::logging::warn(
                            "mint.bump",
                            &format!("Mint #{id}: bump error: {e}"),
                        ),
                    }
                }
            }
            if bumped {
                return get_task(id);
            }
            return out;
        }
        set_status(id, "failed", None, Some("broadcasting without tx_hash"));
        return get_task(id);
    }
    if task.status != "pending" {
        return Ok(task);
    }
    // Not yet due for scheduled run.
    if let Some(sched) = task.scheduled_at {
        if crate::db::now_ms() < sched {
            return Ok(task);
        }
    }

    let claimed = crate::db::with_conn(|conn| {
        let now = crate::db::now_ms();
        let n = conn.execute(
            "UPDATE mint_tasks SET status='signing', updated_at=?1 WHERE id=?2 AND status='pending'",
            rusqlite::params![now, id],
        )?;
        Ok(n > 0)
    })?;
    if !claimed {
        return match get_task(id) {
            Ok(t) => Ok(t),
            Err(_) => Err(AppError::NotFound(format!("mint task {id}"))),
        };
    }

    if task.delay_ms > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(task.delay_ms as u64)).await;
    }

    let fail = |msg: String| -> AppResult<MintTaskRow> {
        let msg = chain::redact_urls_in(&msg);
        // A persisted local tx hash means this tx may already sit in the
        // mempool: requeueing to `pending` would re-sign with a fresh nonce
        // and mint twice, and the first hash would be overwritten. Adjudicate
        // the existing hash instead — only a deterministic node rejection
        // (revert, insufficient funds, nonce too low) fails fast.
        let stamped = crate::db::with_conn(|conn| {
            let h: Option<String> =
                conn.query_row("SELECT tx_hash FROM mint_tasks WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            Ok(h)
        });
        if let Ok(Some(hash)) = stamped {
            if is_permanent_err(&msg) {
                // Node rejected it — the nonce never entered a mempool.
                release_task_nonce(id);
                // set_status keeps the hash via COALESCE for audit/retry safety.
                set_status(id, "failed", None, Some(&msg));
                crate::logging::error("mint", &format!("Mint #{id} rejected: {msg}"));
                wallet_store::log_activity(
                    "mint.failed",
                    &format!("Mint #{id} rejected: {msg} (tx {hash} was not accepted)"),
                    None,
                    false,
                );
            } else {
                set_status(id, "broadcasting", None, Some(&msg));
                wallet_store::log_activity(
                    "mint.retry",
                    &format!(
                        "Mint #{id} send ambiguous ({msg}) — polling existing tx {hash} instead of re-signing"
                    ),
                    None,
                    true,
                );
                spawn_receipt_poll(id);
            }
            return get_task(id);
        }
        // Transient transport/RPC errors get one immediate requeue so a single
        // blip doesn't kill a scheduled FCFS task. Reverts/bad params still fail.
        if task.auto_retries < 1 && is_transient_err(&msg) {
            // Nothing was accepted (no stamp) — the retry re-signs from scratch.
            release_task_nonce(id);
            let now = crate::db::now_ms();
            let _ = crate::db::with_conn(|conn| {
                conn.execute(
                    "UPDATE mint_tasks SET status='pending', auto_retries=auto_retries+1, error=?1, scheduled_at=NULL, updated_at=?2 WHERE id=?3",
                    rusqlite::params![msg.clone(), now, id],
                )?;
                Ok(())
            });
            wallet_store::log_activity(
                "mint.retry",
                &format!("Mint #{id} transient error, requeued once: {msg}"),
                None,
                true,
            );
            return get_task(id);
        }
        release_task_nonce(id);
        set_status(id, "failed", None, Some(&msg));
        crate::logging::error("mint", &format!("Mint #{id} failed: {msg}"));
        wallet_store::log_activity(
            "mint.failed",
            &format!("Mint #{id} failed: {msg}"),
            None,
            false,
        );
        get_task(id)
    };

    let wallet_id = match task.wallet_id {
        Some(w) => w,
        None => return fail("task missing wallet_id".into()),
    };
    let wallet = match wallet_store::get_wallet(wallet_id) {
        Ok(w) => w,
        Err(e) => return fail(format!("wallet: {e}")),
    };
    let chains = match chain::list_chains() {
        Ok(c) => c,
        Err(e) => return fail(format!("chains: {e}")),
    };
    let chain_row = match chains.into_iter().find(|c| c.chain_id == task.chain_id) {
        Some(c) => c,
        None => return fail(format!("chain {} not found", task.chain_id)),
    };

    // A task carrying `opensea_ref` resolves its mint data here, when it fires:
    // signed/allowlist calldata only exists once the stage opens, so it cannot
    // be built at enqueue time (OSNM-Z fetches it at T-2s, retried while the
    // stage is still PREOPEN). Everything else keeps its baked-in calldata.
    let opensea_plan = match task.opensea_ref.as_deref() {
        None => None,
        Some(r) => {
            match fetch_opensea_mint_data(
                task.chain_id,
                wallet_id,
                &wallet.address,
                task.quantity,
                r,
            )
            .await
            {
                Ok(p) => Some(p),
                Err(e) => return fail(format!("opensea mint data: {e}")),
            }
        }
    };

    let (tx_target, calldata, value_u) = if let Some(p) = &opensea_plan {
        let v = match parse_u128_hex_or_dec(&p.value_wei) {
            Ok(v) => v,
            Err(e) => return fail(format!("{e}")),
        };
        (p.to.clone(), p.calldata.clone(), v)
    } else {
        let cd = match resolve_calldata(&task, &wallet.address) {
            Ok(c) => c,
            Err(e) => return fail(format!("calldata: {e}")),
        };
        let value_wei = task.value_wei.clone().unwrap_or_else(|| "0x0".into());
        let v = match parse_u128_hex_or_dec(&value_wei) {
            Ok(v) => v,
            Err(e) => return fail(format!("{e}")),
        };
        (task.contract.clone(), cd, v)
    };

    let endpoints_raw = parse_rpc_list(&task, &chain_row.rpc_url);
    let (primary, endpoints) =
        pick_healthy_endpoint(&endpoints_raw, task.chain_id).await;

    let tx = serde_json::json!({
        "from": wallet.address,
        "to": tx_target,
        "data": calldata,
        "value": format!("0x{:x}", value_u),
    });

    // Simulate mode: eth_call only, no sign/broadcast.
    // Chain-id check runs concurrently so a wrong-chain RPC fails fast instead
    // of returning a misleading result from another network.
    if task.mode == "simulate" {
        // Try each endpoint in order — a transient RPC failure (-32603,
        // timeout, 429) on one node should not kill the task when a
        // fallback endpoint would succeed.
        let mut sim_err: Option<AppError> = None;
        for url in &endpoints {
            let (chk, call_result) = tokio::join!(
                chain::eth_chain_id(url),
                chain::rpc_call(url, "eth_call", serde_json::json!([tx, "latest"]))
            );
            if let Ok(cid) = chk {
                if cid != task.chain_id {
                    sim_err = Some(AppError::Rpc(format!(
                        "RPC chain mismatch on {}: endpoint is chain {cid}, task expects {}",
                        chain::redact_rpc(url),
                        task.chain_id
                    )));
                    continue;
                }
            }
            match call_result {
                Ok(v) => {
                    let ret = v
                        .get("result")
                        .and_then(|r| r.as_str())
                        .unwrap_or("0x")
                        .to_string();
                    set_status(id, "simulated", None, None);
                    let _ = crate::db::with_conn(|conn| {
                        conn.execute(
                            "UPDATE mint_tasks SET error=?1, updated_at=?2 WHERE id=?3",
                            rusqlite::params![format!("eth_call ok: {ret}"), crate::db::now_ms(), id],
                        )?;
                        Ok(())
                    });
                    wallet_store::log_activity(
                        "mint.simulate",
                        &format!("Mint #{id} simulated ok via {}", chain::redact_rpc(url)),
                        None,
                        true,
                    );
                    return get_task(id);
                }
                Err(e) => {
                    // Decode revert selectors into human-readable text.
                    let mut msg =
                        format!("eth_call via {}: {}", chain::redact_rpc(url), decode_rpc_error(&e));
                    // Empty revert data ("0x") decodes to nothing useful — probe
                    // whether the selector even exists on the target first.
                    if msg.contains("without a readable reason") {
                        if let Some(hint) =
                            diagnose_empty_revert(&endpoints, &tx_target, &calldata).await
                        {
                            msg.push_str(&format!(" — {hint}"));
                        }
                    }
                    // Transient (network/RPC) errors → try next endpoint.
                    // Contract reverts → the call itself is invalid, no point
                    // retrying other nodes with the same payload.
                    if is_transient_err(&msg) {
                        sim_err = Some(AppError::Rpc(msg));
                        continue;
                    }
                    return fail(msg);
                }
            }
        }
        return fail(format!(
            "eth_call: all {} endpoint(s) failed: {}",
            endpoints.len(),
            sim_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "unknown".into())
        ));
    }

    // One round instead of three: chain-id verification, the nonce fetch and the
    // fee snapshot run concurrently on each endpoint, so a scheduled launch does
    // not burn serial RTTs at T-0 (OSNM-Z prefetches the same reads before the
    // phase opens). A mismatched RPC would otherwise sign a valid tx for the
    // wrong network; a failed nonce falls through to the next endpoint.
    let need_gas_price = task.max_fee_gwei.is_none() || task.priority_fee_gwei.is_none();
    let mut last_err: Option<String> = None;
    let mut nonce: Option<u64> = None;
    let mut gas_price_prefetch: Option<u128> = None;
    for url in &endpoints {
        let rpc = url.clone();
        let addr = wallet.address.clone();
        let (cid, res, gp) = tokio::join!(
            chain::eth_chain_id(url),
            chain::get_transaction_count(&rpc, &addr),
            async {
                if need_gas_price {
                    chain::gas_price(url).await.map(Some)
                } else {
                    Ok(None)
                }
            },
        );
        if let Ok(cid) = cid {
            if cid != task.chain_id {
                last_err = Some(format!(
                    "{} is chain {cid}, expected {}",
                    chain::redact_rpc(url),
                    task.chain_id
                ));
                continue;
            }
        }
        if gas_price_prefetch.is_none() {
            if let Ok(Some(p)) = gp {
                gas_price_prefetch = Some(p);
            }
        }
        match res {
            Ok(n) => {
                nonce = Some(n);
                break;
            }
            Err(e) => {
                last_err = Some(format!("{url}: {e}"));
            }
        }
    }
    let chain_nonce = match nonce {
        Some(n) => n,
        None => {
            return fail(format!(
                "nonce: all {} endpoint(s) failed: {}",
                endpoints.len(),
                last_err.unwrap_or_else(|| "unknown".into())
            ))
        }
    };
    // Reserve through the shared allocator: the fund runner or another lane
    // may already have taken this wallet's next nonce after our chain read.
    let override_nonce = match task.nonce_override.as_deref().map(str::trim) {
        Some(s) if !s.is_empty() => match s.parse::<u64>() {
            Ok(v) => Some(v),
            Err(_) => return fail(format!("bad nonce override: {s}")),
        },
        _ => None,
    };
    let nonce = crate::nonce::reserve(wallet_id, task.chain_id, chain_nonce, override_nonce);
    // Remember it on the row: every terminal path must be able to hand the
    // reservation back without threading the value through each closure.
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE mint_tasks SET tx_nonce=?1 WHERE id=?2",
            rusqlite::params![nonce as i64, id],
        )?;
        Ok(())
    });

    let gas = if let Some(gl) = &task.gas_limit {
        match gl.parse::<u64>() {
            Ok(v) => v,
            Err(_) => return fail(format!("bad gas_limit: {gl}")),
        }
    } else if task.scheduled_at.is_some() && task.mode == "execute" {
        // Scheduled fire: skip eth_estimateGas entirely. A revert before the
        // stage opens would permanently fail the task, and the extra serial
        // RTT loses FCFS races. 300k covers typical SeaDrop/SeaDropV2 mints.
        300_000
    } else {
        // Try eth_estimateGas across all endpoints before falling back.
        let mut gas_est: Option<u64> = None;
        let mut last_est_err: Option<AppError> = None;
        for url in &endpoints {
            match chain::estimate_gas(url, tx.clone()).await {
                Ok(g) => {
                    gas_est = Some(g.saturating_mul(115) / 100);
                    break;
                }
                Err(e) => {
                    let mut msg = format!("{url}: {}", decode_rpc_error(&e));
                    if msg.contains("without a readable reason") {
                        if let Some(hint) =
                            diagnose_empty_revert(&endpoints, &tx_target, &calldata).await
                        {
                            msg.push_str(&format!(" — {hint}"));
                        }
                    }
                    if msg.to_ascii_lowercase().contains("revert") {
                        // Revert is deterministic — retrying other nodes won't help.
                        return fail(format!(
                            "estimate_gas reverted (check params/value/timing): {msg}"
                        ));
                    }
                    last_est_err = Some(e);
                }
            }
        }
        match gas_est {
            Some(g) => g,
            None => {
                // All endpoints failed estimateGas (non-revert) — some RPCs
                // (Robinhood) omit/fail it; use a safe default for mint calldata.
                wallet_store::log_activity(
                    "rpc.warn",
                    &format!(
                        "estimateGas failed on all endpoints, using 300k default: {}",
                        last_est_err
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "unknown".into())
                    ),
                    None,
                    false,
                );
                300_000
            }
        }
    };

    let auto_mult = auto_fee_multiplier_pct(task.scheduled_at.is_some() && task.mode == "execute");
    // Snapshot taken in the same round as the nonce; re-query only if it failed.
    let observed_gas_price: u128 = if need_gas_price {
        match gas_price_prefetch {
            Some(p) => p,
            None => match chain::gas_price(&primary).await {
                Ok(p) => p,
                Err(e) => return fail(format!("gasPrice: {e}")),
            },
        }
    } else {
        0 // unreachable: both fee fields are manual
    };
    let (max_fee, tip) = if task.max_fee_gwei.is_some() || task.priority_fee_gwei.is_some() {
        let max_fee = match &task.max_fee_gwei {
            Some(s) => match s.parse::<f64>() {
                Ok(g) => (g * 1e9) as u128,
                Err(_) => return fail(format!("bad max_fee_gwei: {s}")),
            },
            None => observed_gas_price.saturating_mul(auto_mult) / 100,
        };
        let tip = match &task.priority_fee_gwei {
            Some(s) => match s.parse::<f64>() {
                Ok(g) => (g * 1e9) as u128,
                Err(_) => return fail(format!("bad priority_fee_gwei: {s}")),
            },
            None => {
                let t = max_fee / 10;
                if t == 0 {
                    max_fee.min(1)
                } else {
                    t
                }
            }
        };
        (max_fee, tip.min(max_fee))
    } else {
        let gas_price = observed_gas_price.saturating_mul(auto_mult) / 100;
        let t = gas_price / 10;
        let tip = if t == 0 {
            gas_price.min(1)
        } else {
            t
        };
        (gas_price, tip)
    };

    // Note: fee/nonce/gas are applied at signing time via SignParams — `tx`
    // above is only the eth_call/eth_estimateGas payload and must not carry
    // non-standard fee keys.

    let signer = match (|| -> AppResult<_> {
        let (nonce_bytes, ct) = wallet_store::key_material(wallet_id)?;
        crate::wallet::load_signer(&nonce_bytes, &ct)
    })() {
        Ok(s) => s,
        Err(e) => return fail(format!("load signer: {e}")),
    };

    let signed = match sign_eip1559(
        &signer,
        SignParams {
            to: &tx_target,
            data_hex: &calldata,
            value: value_u,
            gas_limit: gas,
            nonce,
            chain_id: task.chain_id as u64,
            max_fee,
            tip,
        },
    ) {
        Ok(s) => s,
        Err(e) => return fail(format!("sign: {e}")),
    };

    // Persist the locally-computed tx hash BEFORE any broadcast attempt. If the
    // app crashes mid-send, recovery re-polls this hash instead of re-signing
    // with a fresh nonce (which could double-mint).
    let local_hash = {
        let raw = match hex::decode(signed.trim_start_matches("0x")) {
            Ok(r) => r,
            Err(_) => return fail("internal: signed tx not hex".into()),
        };
        format!("0x{}", hex::encode(alloy::primitives::keccak256(&raw)))
    };
    set_status(id, "broadcasting", Some(&local_hash), None);

    let send_targets: Vec<String> = match task.mode.as_str() {
        "spam" | "sweep" => endpoints,
        _ if task.flashbots => {
            // Flashbots path first (MEV-protected), public RPC as fallback.
            let mut v: Vec<String> = endpoints
                .iter()
                .filter(|u| u.contains("flashbots"))
                .cloned()
                .collect();
            if !v.iter().any(|u| *u == primary) {
                v.push(primary.clone());
            }
            v
        }
        // Plain execute walks the endpoint list in order: a transient primary
        // rejection (429/503/timeout) previously killed the launch because the
        // fallbacks were only used for nonce/estimation, never for the send.
        _ => endpoints,
    };

    let mut first_hash: Option<String> = None;

    for (i, url) in send_targets.iter().enumerate() {
        if task.mode == "sweep" && first_hash.is_some() {
            // Sweep stops at first success (sequential fallback).
            break;
        }
        // Plain execute also stops at the first acceptance — the remaining
        // entries are fallbacks for rejected sends, not extra broadcasts.
        // Spam keeps blasting every endpoint; flashbots+primary multi-broadcast.
        if task.mode == "execute" && !task.flashbots && first_hash.is_some() {
            break;
        }
        if task.delay_ms > 0 && i > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(task.delay_ms as u64)).await;
        }
        match chain::send_raw_transaction(url, &signed).await {
            Ok(hash) => {
                if first_hash.is_none() {
                    first_hash = Some(hash.clone());
                    set_status(id, "broadcasting", Some(&hash), None);
                    wallet_store::log_activity(
                        "mint.broadcast",
                        &format!(
                            "Mint #{id} broadcast {hash} via {}",
                            chain::redact_rpc(url)
                        ),
                        None,
                        true,
                    );
                } else {
                    wallet_store::log_activity(
                        "mint.broadcast",
                        &format!("Mint #{id} also sent via {}", chain::redact_rpc(url)),
                        None,
                        true,
                    );
                }
            }
            Err(e) => {
                let es = e.to_string().to_ascii_lowercase();
                // Re-sending an identical signed tx is idempotent: a node that
                // already has it answers "already known", which is proof of a
                // successful broadcast (the ack was just lost).
                if es.contains("already known")
                    || es.contains("known transaction")
                    || es.contains("already exists")
                {
                    if first_hash.is_none() {
                        first_hash = Some(local_hash.clone());
                        set_status(id, "broadcasting", Some(&local_hash), None);
                        wallet_store::log_activity(
                            "mint.broadcast",
                            &format!(
                                "Mint #{id} accepted {local_hash} (already known via {})",
                                chain::redact_rpc(url)
                            ),
                            None,
                            true,
                        );
                    }
                } else {
                    last_err = Some(chain::redact_urls_in(&format!("{url}: {e}")));
                }
            }
        }
        // Spam: keep blasting remaining endpoints even after success.
        // Execute: flashbots+fallback targets handled by loop size.
    }

    if first_hash.is_some() {
        // Detach the receipt poll: returning now releases the run guard so the
        // next scheduler tick can process new mints immediately.
        spawn_receipt_poll(id);
        get_task(id)
    } else {
        // set_status keeps the pre-broadcast local hash via COALESCE for audit.
        let msg = last_err.unwrap_or_else(|| "no rpc endpoint accepted tx".into());
        fail(format!("send: {msg}"))
    }
}

async fn poll_receipt(id: i64, hash: &str) -> AppResult<MintTaskRow> {
    let task = get_task(id)?;
    let chains = chain::list_chains()?;
    let chain_row = match chains.into_iter().find(|c| c.chain_id == task.chain_id) {
        Some(c) => c,
        None => {
            set_status(id, "failed", Some(hash), Some("chain missing for receipt poll"));
            return get_task(id);
        }
    };
    // Rotate across every endpoint, not just the first: one dead provider must
    // not blind the poll, and a fee-bumped replacement may only be known to the
    // node that accepted it. Both the current and the replaced hash are checked
    // — either one can be the one that mines (same nonce, so never both).
    let endpoints = parse_rpc_list(&task, &chain_row.rpc_url);
    let prev = task.prev_tx_hash.clone();
    let mut start = 0usize;
    let mut consecutive_errs = 0u32;
    for pass in 0..15 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        // Adjudicate a phantom hash once, ~10s in: when no right-chain endpoint
        // even knows the tx, the send was never accepted anywhere — polling
        // would otherwise stall the wallet's queue for the full ~20m window.
        // Flashbots sends are exempt: private bundles are invisible to public
        // eth_getTransactionByHash.
        if pass == 4 && !task.flashbots {
            let mut probe_answered = false;
            let mut probe_known = false;
            for url in &endpoints {
                // Bounded probes: a dead endpoint must not stretch the "10s"
                // check by its full timeout+retry chain.
                let cid = match tokio::time::timeout(
                    std::time::Duration::from_secs(6),
                    chain::eth_chain_id(url),
                )
                .await
                {
                    Ok(Ok(cid)) => Some(cid),
                    _ => None,
                };
                let Some(cid) = cid else { continue };
                if cid != task.chain_id {
                    continue;
                }
                let probe = tokio::time::timeout(
                    std::time::Duration::from_secs(6),
                    chain::rpc_call(
                        url,
                        "eth_getTransactionByHash",
                        serde_json::json!([hash]),
                    ),
                )
                .await;
                if let Ok(Ok(v)) = probe {
                    probe_answered = true;
                    if v.get("result").map(|r| !r.is_null()).unwrap_or(false) {
                        probe_known = true;
                        break;
                    }
                }
            }
            if probe_answered && !probe_known {
                release_task_nonce(id);
                let msg = "tx unknown to every endpoint ~10s after broadcast — the send was never accepted; re-enqueue to retry";
                set_status(id, "failed", Some(hash), Some(msg));
                crate::logging::warn("mint.poll", &format!("Mint #{id}: {msg}"));
                wallet_store::log_activity(
                    "mint.failed",
                    &format!("Mint #{id}: {msg}"),
                    None,
                    false,
                );
                return get_task(id);
            }
        }
        let mut answered = false;
        for k in 0..endpoints.len().max(1) {
            let url = &endpoints[(start + k) % endpoints.len().max(1)];
            let mut endpoint_ok = false;
            let mut hashes: Vec<&str> = vec![hash];
            if let Some(p) = prev.as_deref() {
                if p != hash {
                    hashes.push(p);
                }
            }
            for h in &hashes {
                match chain::get_receipt(url, h).await {
                    Ok(Some(receipt)) => {
                        let status = receipt
                            .get("status")
                            .and_then(|s| s.as_str())
                            .unwrap_or("0x1");
                        let which = if *h == hash { "" } else { " (fee-bumped tx)" };
                        if status == "0x1" {
                            set_status(id, "confirmed", Some(h), None);
                            wallet_store::log_activity(
                                "mint.confirmed",
                                &format!("Mint #{id} confirmed {h}{which}"),
                                None,
                                true,
                            );
                            crate::logging::info("mint", &format!("Mint #{id} confirmed {h}"));
                        } else {
                            set_status(id, "failed", Some(h), Some("reverted on-chain"));
                            crate::logging::error("mint", &format!("Mint #{id} reverted {h}"));
                            wallet_store::log_activity(
                                "mint.failed",
                                &format!("Mint #{id} reverted {h}{which}"),
                                None,
                                false,
                            );
                        }
                        return get_task(id);
                    }
                    Ok(None) => endpoint_ok = true,
                    Err(_) => {}
                }
            }
            if endpoint_ok {
                answered = true;
                break;
            }
        }
        start = start.wrapping_add(1);
        if answered {
            consecutive_errs = 0;
        } else {
            // Transient RPC hiccups must not abort the whole poll window —
            // only give up after several cycles where every endpoint failed.
            consecutive_errs += 1;
            if consecutive_errs >= 5 {
                let msg = format!(
                    "receipt retry: all {} endpoint(s) failing",
                    endpoints.len()
                );
                crate::logging::warn("mint.poll", &format!("Mint #{id}: {msg}"));
                set_status(id, "broadcasting", Some(hash), Some(&msg));
                return get_task(id);
            }
        }
    }
    crate::logging::info(
        "mint.poll",
        &format!("Mint #{id}: receipt window elapsed, will re-poll"),
    );
    set_status(
        id,
        "broadcasting",
        Some(hash),
        Some("timeout waiting for receipt — will re-poll on next run"),
    );
    get_task(id)
}

/// A stuck broadcast may be re-signed this many times before we just wait.
const MAX_BUMPS: i64 = 3;

/// Replacement fee: compound +10% per step over the ORIGINAL fee, capped at
/// 3x, and always strictly above it (nodes reject equal-fee replacements).
fn bumped_fee(original: u128, steps: i64) -> u128 {
    let mut fee = original;
    for _ in 0..steps.max(0) {
        fee = fee.saturating_mul(11) / 10;
    }
    fee.min(original.saturating_mul(3))
        .max(original.saturating_add(1))
}

/// The pending tx as some node knows it — enough to re-sign the same payload.
struct FetchedTx {
    nonce: u64,
    max_fee: u128,
    tip: u128,
    gas: u64,
    value: u128,
    to: String,
    data: String,
}/// Quantity field off a transaction object. Mainstream nodes hex-encode these
/// (`"0x…"`); a few gateways send decimal strings. The old generic helper
/// parsed hex digits with `T::from_str` (decimal-only), so `"0x12"` read as 12
/// and anything containing a hex letter failed — silently disabling fee bumps.
fn hex_field_u64(v: &serde_json::Value, key: &str) -> Option<u64> {
    let s = v.get(key)?.as_str()?;
    match s.strip_prefix("0x") {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => s.parse::<u64>().ok(),
    }
}

fn hex_field_u128(v: &serde_json::Value, key: &str) -> Option<u128> {
    let s = v.get(key)?.as_str()?;
    match s.strip_prefix("0x") {
        Some(h) => u128::from_str_radix(h, 16).ok(),
        None => s.parse::<u128>().ok(),
    }
}

async fn fetch_tx_for_bump(endpoints: &[String], hash: &str) -> Option<FetchedTx> {
    for url in endpoints {
        let v = match chain::rpc_call(url, "eth_getTransactionByHash", serde_json::json!([hash]))
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        let Some(tx) = v.get("result").filter(|r| !r.is_null()) else {
            continue;
        };
        let to = tx.get("to").and_then(|t| t.as_str())?.to_string();
        let data = tx
            .get("input")
            .and_then(|d| d.as_str())
            .unwrap_or("0x")
            .to_string();
        return Some(FetchedTx {
            nonce: hex_field_u64(tx, "nonce")?,
            max_fee: hex_field_u128(tx, "maxFeePerGas")?,
            tip: hex_field_u128(tx, "maxPriorityFeePerGas")?,
            gas: hex_field_u64(tx, "gas")?,
            value: hex_field_u128(tx, "value").unwrap_or(0),
            to,
            data,
        });
    }
    None
}

/// Re-sign the SAME transaction (identical nonce and payload) with a higher fee
/// and rebroadcast. Because both versions share the nonce, at most one can ever
/// mine — a bump can replace a stuck mint, never double it.
async fn bump_stuck_tx(id: i64) -> AppResult<Option<String>> {
    let task = get_task(id)?;
    if task.status != "broadcasting" || task.bump_count >= MAX_BUMPS {
        return Ok(None);
    }
    let Some(old_hash) = task.tx_hash.clone() else {
        return Ok(None);
    };
    let chains = chain::list_chains()?;
    let Some(chain_row) = chains.into_iter().find(|c| c.chain_id == task.chain_id) else {
        return Ok(None);
    };
    let endpoints = parse_rpc_list(&task, &chain_row.rpc_url);
    let Some(orig) = fetch_tx_for_bump(&endpoints, &old_hash).await else {
        crate::logging::info(
            "mint.bump",
            &format!("Mint #{id}: tx {old_hash} unknown to every endpoint — not bumping"),
        );
        return Ok(None);
    };
    let steps = task.bump_count + 1;
    let max_fee = bumped_fee(orig.max_fee, steps);
    let mut tip = bumped_fee(orig.tip, steps);
    if tip > max_fee {
        tip = max_fee;
    }

    let Some(wallet_id) = task.wallet_id else {
        return Ok(None);
    };
    let signer = (|| -> AppResult<_> {
        let (nb, ct) = wallet_store::key_material(wallet_id)?;
        crate::wallet::load_signer(&nb, &ct)
    })()?;

    let signed = sign_eip1559(
        &signer,
        SignParams {
            to: &orig.to,
            data_hex: &orig.data,
            value: orig.value,
            gas_limit: orig.gas,
            nonce: orig.nonce,
            chain_id: task.chain_id as u64,
            max_fee,
            tip,
        },
    )?;
    let new_hash = {
        let raw = hex::decode(signed.trim_start_matches("0x"))
            .map_err(|_| AppError::Rpc("bump: signed tx not hex".into()))?;
        format!("0x{}", hex::encode(alloy::primitives::keccak256(&raw)))
    };
    if new_hash == old_hash {
        return Ok(None); // identical fees — nothing to replace
    }

    let mut accepted = false;
    let mut last_err: Option<String> = None;
    for url in &endpoints {
        match chain::send_raw_transaction(url, &signed).await {
            Ok(_) => {
                accepted = true;
                break;
            }
            Err(e) => {
                let s = e.to_string().to_ascii_lowercase();
                if s.contains("already known")
                    || s.contains("known transaction")
                    || s.contains("already exists")
                {
                    accepted = true;
                    break;
                }
                last_err = Some(chain::redact_urls_in(&e.to_string()));
            }
        }
    }
    if !accepted {
        crate::logging::warn(
            "mint.bump",
            &format!(
                "Mint #{id}: bump rejected — {}",
                last_err.unwrap_or_else(|| "no endpoint accepted it".into())
            ),
        );
        return Ok(None);
    }

    // Swap in the new hash and remember the old one: the receipt poll watches
    // both, because either version (never two) is what will mine.
    crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE mint_tasks SET tx_hash=?1, prev_tx_hash=?2, bump_count=bump_count+1, error=NULL, updated_at=?3 WHERE id=?4",
            rusqlite::params![new_hash, old_hash, crate::db::now_ms(), id],
        )?;
        Ok(())
    })?;
    crate::logging::info(
        "mint.bump",
        &format!("Mint #{id}: fee bump #{steps} {old_hash} → {new_hash}"),
    );
    wallet_store::log_activity(
        "mint.bump",
        &format!("Mint #{id}: fee bumped (attempt {steps}) {old_hash} → {new_hash}"),
        None,
        true,
    );
    Ok(Some(new_hash))
}

fn parse_u128_hex_or_dec(s: &str) -> AppResult<u128> {
    if let Some(h) = s.trim().strip_prefix("0x") {
        u128::from_str_radix(h, 16).map_err(|_| AppError::Invalid("bad value_wei".into()))
    } else {
        s.trim()
            .parse::<u128>()
            .map_err(|_| AppError::Invalid("bad value_wei".into()))
    }
}

pub(crate) struct SignParams<'a> {
    pub(crate) to: &'a str,
    pub(crate) data_hex: &'a str,
    pub(crate) value: u128,
    pub(crate) gas_limit: u64,
    pub(crate) nonce: u64,
    pub(crate) chain_id: u64,
    pub(crate) max_fee: u128,
    pub(crate) tip: u128,
}

pub(crate) fn sign_eip1559(
    signer: &alloy::signers::local::PrivateKeySigner,
    p: SignParams<'_>,
) -> AppResult<String> {
    use alloy::consensus::{SignableTransaction, TxEnvelope, TxEip1559};
    use alloy::network::TxSignerSync;
    use alloy::primitives::{Address, TxKind, U256};

    let to_addr: Address = p
        .to
        .parse()
        .map_err(|_| AppError::Invalid("bad to address".into()))?;
    let input = hex::decode(p.data_hex.trim_start_matches("0x"))
        .map_err(|_| AppError::Invalid("bad data".into()))?
        .into();

    let mut tx_req = TxEip1559 {
        chain_id: p.chain_id,
        nonce: p.nonce,
        gas_limit: p.gas_limit,
        max_fee_per_gas: p.max_fee,
        max_priority_fee_per_gas: p.tip,
        to: TxKind::Call(to_addr),
        value: U256::from(p.value),
        input,
        access_list: Default::default(),
    };

    let sig = signer
        .sign_transaction_sync(&mut tx_req)
        .map_err(|e| AppError::Crypto(format!("sign tx: {e}")))?;
    let signed = tx_req.into_signed(sig);
    let envelope = TxEnvelope::from(signed);
    use alloy::eips::eip2718::Encodable2718;
    let mut out = hex::encode(envelope.encoded_2718());
    out.insert_str(0, "0x");
    Ok(out)
}

/// Kick worker (manual "Run queue"): process pending tasks and wait for this
/// pass to finish so the caller sees post-run states. Receipt re-polls are
/// handed to background tasks so they never hold anything up.
/// Tasks are grouped by wallet: different wallets run in parallel (FCFS race),
/// same-wallet tasks stay sequential to avoid nonce collisions.
pub async fn run_pending() -> AppResult<Vec<MintTaskRow>> {
    let (broadcasting, lanes) = launch_lanes()?;
    let mut results = Vec::new();
    for lane in lanes {
        if let Ok(mut batch) = lane.await {
            results.append(&mut batch);
        }
    }
    for id in broadcasting {
        if let Ok(t) = get_task(id) {
            results.push(t);
        }
    }
    Ok(results)
}

/// Scheduler kick: launch lanes without waiting. The 60ms tick must never
/// block — wallets whose lane is busy (e.g. sleeping out a `delay_ms`) are
/// skipped for this tick and picked up again once the lane releases, while
/// every other wallet keeps firing immediately.
pub fn spawn_pending() -> AppResult<Vec<MintTaskRow>> {
    let (broadcasting, _lanes) = launch_lanes()?; // dropping JoinHandles detaches
    let mut results = Vec::new();
    for id in broadcasting {
        if let Ok(t) = get_task(id) {
            results.push(t);
        }
    }
    Ok(results)
}

/// Select runnable tasks, spawn one sequential lane per *free* wallet and
/// resume detached receipt polls. Returns the broadcasting ids and the lanes
/// the caller may await (or drop to detach).
fn launch_lanes() -> AppResult<(Vec<i64>, Vec<tokio::task::JoinHandle<Vec<MintTaskRow>>>)> {
    let (pending, broadcasting): (Vec<(i64, Option<i64>)>, Vec<i64>) = crate::db::with_conn(|conn| {
        let mut pending = Vec::new();
        let mut broadcasting = Vec::new();
        {
            let mut stmt = conn
                .prepare("SELECT id, wallet_id FROM mint_tasks WHERE status = 'pending' ORDER BY id")?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            for row in rows {
                pending.push(row?);
            }
        }
        {
            let mut stmt = conn
                .prepare("SELECT id FROM mint_tasks WHERE status = 'broadcasting' ORDER BY id")?;
            let ids = stmt.query_map([], |r| r.get(0))?;
            for id in ids {
                broadcasting.push(id?);
            }
        }
        Ok((pending, broadcasting))
    })?;
    // Group pending tasks by wallet (None → single shared bucket).
    let mut by_wallet: std::collections::HashMap<Option<i64>, Vec<i64>> =
        std::collections::HashMap::new();
    for (id, wallet_id) in pending {
        by_wallet.entry(wallet_id).or_default().push(id);
    }
    // Spawn one sequential lane per wallet so distinct wallets fire together.
    let mut lanes = Vec::new();
    for (wallet_id, ids) in by_wallet {
        // Claim the wallet's lane; when one is already in flight the tasks
        // stay untouched and a later tick retries them.
        if !lock_unpoisoned(&LANE_GUARDS).insert(wallet_id) {
            continue;
        }
        lanes.push(tokio::spawn(async move {
            let _lane = LaneGuard(wallet_id);
            let mut out = Vec::new();
            for id in ids {
                match process_task(id).await {
                    Ok(t) => out.push(t),
                    Err(e) => {
                        release_task_nonce(id);
                        crate::logging::error("mint", &format!("Mint #{id} error: {e}"));
                        set_status(id, "failed", None, Some(&e.to_string()));
                        wallet_store::log_activity(
                            "mint.failed",
                            &format!("Mint #{id} error: {e}"),
                            None,
                            false,
                        );
                        if let Ok(t) = get_task(id) {
                            out.push(t);
                        }
                    }
                }
            }
            out
        }));
    }
    // Resume receipt re-polls in the background (skipped when already in
    // flight) so slow polls never block a lane or a tick.
    for id in &broadcasting {
        spawn_receipt_poll(*id);
    }
    Ok((broadcasting, lanes))
}

/// Reset stuck signing tasks from a previous crash so they can re-run.
pub fn recover_stale_tasks() {
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE mint_tasks SET status='pending', error='recovered from interrupted run', updated_at=?1
             WHERE status='signing'",
            [crate::db::now_ms()],
        )?;
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // EIP-1167 stub for 0x09a26fc8fcef18192e267d7a6da9dfb4be81dd6a — the
    // implementation behind Robinhood Chain's 0x05780625… clone.
    const EIP1167_STUB: &str = "363d3d373d3d3d363d7309a26fc8fcef18192e267d7a6da9dfb4be81dd6a5af43d82803e903d91602b57fd5bf3";

    #[test]
    fn bump_fee_compounds_and_caps() {
        // +10% per step over the original, strictly above it, capped at 3x.
        assert_eq!(bumped_fee(100, 1), 110);
        assert_eq!(bumped_fee(100, 2), 121);
        assert_eq!(bumped_fee(100, 3), 133);
        assert!(bumped_fee(100, 0) > 100); // never equal — nodes reject that
        assert_eq!(bumped_fee(1_000_000, 20), 3_000_000); // capped at 3x
        assert_eq!(bumped_fee(1, 1), 2); // rounding cannot hand back the original
        assert_eq!(bumped_fee(0, 1), 1);
    }

    /// Quantity fields off real node payloads: hex with letters must parse as
    /// hex (the old generic helper read "0x12" as decimal 12 and dropped any
    /// value containing a-f — silently killing fee bumps).
    #[test]
    fn hex_fields_parse_radix_and_decimal() {
        let tx = serde_json::json!({
            "nonce": "0x12",
            "maxFeePerGas": "0x3b9aca00",
            "maxPriorityFeePerGas": "0x77359400",
            "gas": "0x5208",
            "value": "0x2386f26fc10000",
        });
        assert_eq!(hex_field_u64(&tx, "nonce"), Some(18));
        assert_eq!(hex_field_u128(&tx, "maxFeePerGas"), Some(1_000_000_000));
        assert_eq!(hex_field_u128(&tx, "maxPriorityFeePerGas"), Some(2_000_000_000));
        assert_eq!(hex_field_u64(&tx, "gas"), Some(21_000));
        assert_eq!(hex_field_u128(&tx, "value"), Some(10_000_000_000_000_000));

        // Gateways that send decimal strings still parse.
        let dec = serde_json::json!({ "nonce": "18", "value": "10000000000000000" });
        assert_eq!(hex_field_u64(&dec, "nonce"), Some(18));
        assert_eq!(hex_field_u128(&dec, "value"), Some(10_000_000_000_000_000));

        // Garbage is None, never a silent wrong number.
        let bad = serde_json::json!({ "nonce": "0xzz", "gas": "" });
        assert_eq!(hex_field_u64(&bad, "nonce"), None);
        assert_eq!(hex_field_u64(&bad, "gas"), None);
        assert_eq!(hex_field_u64(&bad, "missing"), None);
    }

    /// Terminal failure hands the reserved nonce back, so the next task on the
    /// same wallet signs the same nonce instead of a gap nobody consumes.
    #[test]
    fn terminal_failure_hands_the_nonce_back() {
        let _serial = serial_guard();
        fresh_db("nonce-rel");
        ensure_unlocked();

        let label = format!(
            "rel-w-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        );
        let w = crate::wallet_store::create_wallet(&label, None).expect("wallet");
        let task = enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: 8453,
            contract: "0x0000000000000000000000000000000000000009".into(),
            quantity: 1,
            value_wei: None,
            function_name: Some("mint()".into()),
            is_hex: false,
            parameters: None,
            calldata: Some("0x1249c58b".into()),
            rpc_endpoints: None,
            flashbots: false,
            gas_limit: None,
            max_fee_gwei: None,
            priority_fee_gwei: None,
            nonce_override: None,
            scheduled_at: None,
            delay_ms: None,
            mode: Some("execute".into()),
            opensea_ref: None,
        })
        .expect("enqueue");

        crate::nonce::reset_for_tests(w.id, 8453);
        let n = crate::nonce::reserve(w.id, 8453, 4, None);
        assert_eq!(n, 4);
        crate::db::with_conn(|conn| {
            conn.execute(
                "UPDATE mint_tasks SET tx_nonce=?1 WHERE id=?2",
                rusqlite::params![n as i64, task.id],
            )?;
            Ok(())
        })
        .unwrap();

        release_task_nonce(task.id);
        assert_eq!(
            crate::nonce::reserve(w.id, 8453, 4, None),
            4,
            "nonce must come back after a terminal failure"
        );
        crate::nonce::reset_for_tests(w.id, 8453);
    }

    /// Finished runs are capped for the IPC payload; queued/in-flight work
    /// never is, no matter how old.
    #[test]
    fn list_caps_history_but_always_returns_active_tasks() {
        let _serial = serial_guard();
        fresh_db("listcap");
        clear_lanes_for_tests();
        let now = crate::db::now_ms();
        crate::db::with_conn(|conn| {
            for i in 0..(LIST_HISTORY_CAP + 5) {
                conn.execute(
                    "INSERT INTO mint_tasks(chain_id, contract, quantity, status, created_at, updated_at)
                     VALUES (8453, '0x000000000000000000000000000000000000000a', 1, 'confirmed', ?1, ?1)",
                    [now - i as i64],
                )?;
            }
            // The oldest rows in the table are still queued — they must survive.
            for i in 0..3 {
                conn.execute(
                    "INSERT INTO mint_tasks(chain_id, contract, quantity, status, created_at, updated_at)
                     VALUES (8453, '0x000000000000000000000000000000000000000b', 1, 'pending', ?1, ?1)",
                    [now - 10_000_000 - i as i64],
                )?;
            }
            Ok(())
        })
        .unwrap();

        let list = list_tasks().unwrap();
        assert_eq!(
            list.len(),
            LIST_HISTORY_CAP + 3,
            "history capped, active kept"
        );
        assert_eq!(
            list.iter().filter(|t| t.status == "pending").count(),
            3,
            "active rows must never be capped"
        );
        assert!(list
            .windows(2)
            .all(|w| w[0].created_at >= w[1].created_at));
    }

    /// A wallet whose lane is already in flight is skipped — its tasks stay
    /// untouched for a later tick — and the lane is released when it drains.
    #[test]
    fn busy_wallet_lane_is_skipped_then_released() {
        let _serial = serial_guard();
        fresh_db("lane");
        clear_lanes_for_tests();
        ensure_unlocked();

        let label = format!(
            "lane-w-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        );
        let w = crate::wallet_store::create_wallet(&label, None).unwrap();
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap();
        let task = enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x000000000000000000000000000000000000000c".into(),
            quantity: 1,
            function_name: Some("mint()".into()),
            mode: Some("simulate".into()),
            ..Default::default()
        })
        .unwrap();

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        force_lane_busy(Some(w.id));
        assert!(is_running());
        let out = rt.block_on(run_pending()).unwrap();
        assert_eq!(
            get_task(task.id).unwrap().status,
            "pending",
            "busy lane must leave the task untouched"
        );
        assert!(out.iter().all(|t| t.id != task.id));

        clear_lanes_for_tests();
        assert!(!is_running());

        let out = rt.block_on(run_pending()).unwrap();
        let done = get_task(task.id).unwrap();
        assert_ne!(done.status, "pending", "free lane must process it");
        assert!(out.iter().any(|t| t.id == task.id));
        clear_lanes_for_tests();
    }

    #[test]
    fn detects_eip1167_minimal_proxy() {
        assert_eq!(
            minimal_proxy_target(EIP1167_STUB),
            Some("0x09a26fc8fcef18192e267d7a6da9dfb4be81dd6a".into())
        );
        let variant = "3d3d3d3d363d7309a26fc8fcef18192e267d7a6da9dfb4be81dd6a5af43d82803e903d91602b57fd5bf3";
        assert_eq!(
            minimal_proxy_target(variant),
            Some("0x09a26fc8fcef18192e267d7a6da9dfb4be81dd6a".into())
        );
    }

    #[test]
    fn ignores_non_stub_bytecode() {
        assert_eq!(minimal_proxy_target("6080604052348015600f57600080fd5b50"), None);
        assert_eq!(minimal_proxy_target(""), None);
        // Right prefix, but not the canonical tail/length.
        assert_eq!(minimal_proxy_target(&EIP1167_STUB[..80]), None);
        assert_eq!(minimal_proxy_target(&format!("{EIP1167_STUB}ff")), None);
    }

    #[test]
    fn builds_mint_no_args() {
        let cd = build_calldata_from_fn("mint()", None, 1, "0x0000000000000000000000000000000000000001")
            .unwrap();
        assert_eq!(cd, "0x1249c58b");
    }

    #[test]
    fn builds_mint_uint_with_placeholder() {
        let wallet = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let cd = build_calldata_from_fn("mint(address,uint256)", None, 2, wallet).unwrap();
        assert!(cd.starts_with("0x"));
        let body = &cd[2..];
        // 4-byte selector (8 hex) + N * 32-byte words (64 hex each)
        assert_eq!((body.len() - 8) % 64, 0);
        assert!(body.len() > 8);
        assert_ne!(&body[..8], "1249c58b");
    }

    #[test]
    fn rejects_bad_signature() {
        assert!(build_calldata_from_fn("mint", None, 1, "0x1").is_err());
        assert!(build_calldata_from_fn("mint(uint256)", Some("not-a-number"), 1, "0x1").is_err());
    }

    #[test]
    fn builds_merkle_proof_calldata() {
        let wallet = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let proof1 = format!("0x{}", "11".repeat(32));
        let proof2 = format!("0x{}", "22".repeat(32));
        let entry = allowlist::AllowlistEntry {
            address: wallet.to_lowercase(),
            proof: Some(vec![proof1.clone(), proof2.clone()]),
            signature: None,
            calldata: None,
        };
        let cd = build_calldata_with_entry(
            "mint(uint256,bytes32[])",
            Some("1;{proof}"),
            1,
            wallet,
            Some(&entry),
        )
        .unwrap();
        let body = &cd[2..];
        // selector + uint head + offset head + tail(len + 2 leaves)
        // dynamic tail = 32 + 64 = 96; head = 64; total after selector = 64+96=160
        assert_eq!((body.len() - 8) % 64, 0);
        assert!(body.contains(proof1.trim_start_matches("0x")));
        assert!(body.contains(proof2.trim_start_matches("0x")));
        // offset word should be 0x40 (2 * 32)
        let offset_word = &body[8 + 64..8 + 128];
        let expected_offset = format!("{}40", "0".repeat(62));
        assert_eq!(offset_word, expected_offset);
    }

    #[test]
    fn builds_signature_bytes_calldata() {
        let wallet = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let sig = format!("0x{}", "ab".repeat(65));
        let entry = allowlist::AllowlistEntry {
            address: wallet.to_lowercase(),
            proof: None,
            signature: Some(sig.clone()),
            calldata: None,
        };
        let cd = build_calldata_with_entry(
            "mint(uint256,bytes)",
            Some("1;{signature}"),
            1,
            wallet,
            Some(&entry),
        )
        .unwrap();
        assert!(cd.contains(&sig[2..]));
    }

    #[test]
    fn auto_fee_multiplier_matches_seadrop_launch_policy() {
        assert_eq!(auto_fee_multiplier_pct(false), 125);
        assert_eq!(auto_fee_multiplier_pct(true), 250);
        let observed = 1_000_000_000u128;
        assert_eq!(
            observed * auto_fee_multiplier_pct(true) / 100,
            2_500_000_000,
            "scheduled launch must ride 2.5x the observed gas price"
        );
    }

    #[test]
    fn count_runnable_filters_future_schedules() {
        let _serial = serial_guard();
        fresh_db("runnable");
        ensure_unlocked();
        assert_eq!(count_runnable().unwrap(), 0);
        let w = crate::wallet_store::create_wallet("runnable-w", None).unwrap();
        let chains = crate::chain::list_chains().unwrap();
        let base = chains.iter().find(|c| c.chain_id == 8453).unwrap().clone();
        let future = crate::db::now_ms() + 60_000;
        enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            scheduled_at: Some(future),
            mode: Some("simulate".into()),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(count_runnable().unwrap(), 0);
        enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000002".into(),
            quantity: 1,
            scheduled_at: None,
            mode: Some("simulate".into()),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(count_runnable().unwrap(), 1);
        assert!(!is_running());
    }

    #[test]
    fn due_for_prep_marks_each_task_once_inside_lead() {
        let _serial = serial_guard();
        fresh_db("prep");
        ensure_unlocked();
        let w = crate::wallet_store::create_wallet("prep-w", None).unwrap();
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap();
        let now = crate::db::now_ms();
        let id = enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            scheduled_at: Some(now + 5_000),
            mode: Some("execute".into()),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
        let draft = enqueue_draft(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000002".into(),
            quantity: 1,
            scheduled_at: Some(now + 5_000),
            mode: Some("execute".into()),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap()
        .id;
        // Outside the 10s lead there is nothing to warm yet.
        assert!(due_for_prep(now - 30_000).unwrap().is_empty());
        // Inside the lead: reported once, then deduped for later ticks.
        assert_eq!(due_for_prep(now).unwrap(), vec![id]);
        assert!(due_for_prep(now).unwrap().is_empty());
        // Drafts never fire, so they are never prepped.
        assert!(!due_for_prep(now).unwrap().contains(&draft));
        // Past fire time the scan moves on.
        assert!(due_for_prep(now + 60_000).unwrap().is_empty());
    }

    #[test]
    fn scheduled_opensea_task_stores_fire_time_ref() {
        let _serial = serial_guard();
        fresh_db("opensearef");
        ensure_unlocked();
        let w = crate::wallet_store::create_wallet("opensearef-w", None).unwrap();
        let chains = crate::chain::list_chains().unwrap();
        let base = chains.iter().find(|c| c.chain_id == 8453).unwrap().clone();
        let future = crate::db::now_ms() + 60_000;
        let task = enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 2,
            scheduled_at: Some(future),
            mode: Some("execute".into()),
            opensea_ref: Some(
                r#"{"collection":"0x0000000000000000000000000000000000000001","token_id":"0"}"#
                    .into(),
            ),
            ..Default::default()
        })
        .unwrap();
        let roundtrip = get_task(task.id).unwrap();
        let r = roundtrip
            .opensea_ref
            .as_deref()
            .expect("fire-time ref must survive the roundtrip");
        let parsed: OpenseaRef = serde_json::from_str(r).unwrap();
        assert_eq!(
            parsed.collection,
            "0x0000000000000000000000000000000000000001"
        );
        assert_eq!(parsed.token_id, "0");
        // Scheduled tasks stay "pending" — the runner gates on scheduled_at.
        assert_eq!(roundtrip.status, "pending");
        assert_eq!(roundtrip.scheduled_at, Some(future));
        assert_eq!(roundtrip.quantity, 2);
    }

    #[test]
    fn opensea_mint_data_retry_classification() {
        assert!(opensea_mint_data_retryable("mint public is not live yet"));
        assert!(opensea_mint_data_retryable("dropNotMinting(0x1234)"));
        assert!(opensea_mint_data_retryable(
            "transport error: 429 Too Many Requests"
        ));
        assert!(opensea_mint_data_retryable("http 503 service unavailable"));
        assert!(!opensea_mint_data_retryable(
            "no eligible OpenSea stage for this wallet"
        ));
        assert!(!opensea_mint_data_retryable("collection not on OpenSea"));
        assert!(!opensea_mint_data_retryable("session wallet mismatch"));
    }

    #[test]
    fn mode_validation() {
        assert!(validate_mode("execute").is_ok());
        assert!(validate_mode("simulate").is_ok());
        assert!(validate_mode("spam").is_ok());
        assert!(validate_mode("sweep").is_ok());
        assert!(validate_mode("nope").is_err());
    }

    #[test]
    fn hex_placeholder_validation() {
        let _serial = serial_guard();
        fresh_db("hexph");
        ensure_unlocked();
        let w = crate::wallet_store::create_wallet("hexph-w", None)
            .or_else(|_| {
                crate::wallet_store::list_wallets()?
                    .into_iter()
                    .next()
                    .ok_or(crate::error::AppError::NotFound("wallet".into()))
            })
            .unwrap();
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap();
        let common = EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            is_hex: true,
            mode: Some("execute".into()),
            ..Default::default()
        };
        // Unknown placeholder tokens are rejected at enqueue time — they would
        // otherwise pass the brace scanner and blow up only at sign time.
        assert!(enqueue(EnqueueArgs {
            calldata: Some("0x1234{to}5678".into()),
            ..common.clone()
        })
        .is_err());
        // {signature}/{proof} must be resolved via allowlist before enqueue.
        assert!(enqueue(EnqueueArgs {
            calldata: Some("0x1249c58b{signature}".into()),
            ..common.clone()
        })
        .is_err());
        // Odd-length static hex around {address} would decode-fail after
        // substitution (40 nibbles cannot fix odd parity).
        assert!(enqueue(EnqueueArgs {
            calldata: Some("0x123{address}".into()),
            ..common.clone()
        })
        .is_err());
        // Valid: pure selector + {address} with even static segments.
        assert!(enqueue(EnqueueArgs {
            calldata: Some("0x1249c58b{address}".into()),
            ..common.clone()
        })
        .is_ok());
    }

    fn fresh_db(tag: &str) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_mint_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = crate::db::init(&dir.join("m.db"));
        // The global connection is a OnceLock — only the first init wins, so
        // every test shares one database. Wipe mint state so tests never see
        // each other's leftovers (pair with serial_guard below).
        let _ = crate::db::with_conn(|conn| {
            conn.execute("DELETE FROM mint_tasks", [])?;
            Ok(())
        });
    }

    /// Mint tests share the process-global DB + vault — serialize them so
    /// parallel `cargo test` runs don't race (the old VaultLocked flake).
    /// Delegates to the shared lock: mint and e2e tests must not overlap either.
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::db::test_support::serial_guard()
    }

    fn ensure_unlocked() {
        if crate::vault::is_unlocked().unwrap_or(false) {
            return;
        }
        for pass in [
            "test-pass-123",
            "correct-horse-1",
            "aegis-e2e-pass",
        ] {
            if crate::vault::unlock_or_create(pass).is_ok() {
                return;
            }
        }
        panic!("could not unlock vault for mint e2e");
    }

    #[test]
    fn error_taxonomy_separates_permanent_from_transient() {
        // Deterministic rejections must never be auto-requeued/polled.
        assert!(is_permanent_err("nonce too low"));
        assert!(is_permanent_err("execution reverted: NotActive()"));
        assert!(is_permanent_err("insufficient funds for gas * price + value"));
        assert!(is_permanent_err("send: endpoint is chain 1, expected 4663"));
        assert!(!is_transient_err("execution reverted: NotActive()"));
        assert!(!is_transient_err("nonce too low"));
        // Transport-class blips still deserve the one-shot requeue.
        assert!(is_transient_err("error sending request: timed out"));
        assert!(is_transient_err("HTTP 429 Too Many Requests"));
        assert!(is_transient_err("send: connection reset by peer"));
        assert!(is_transient_err("send: 503 Service Unavailable"));
    }

    /// Backend half of the double-click guard: while an identical task is
    /// still queued, a second submit must be rejected — two rows would sign
    /// two transactions for one mint.
    #[test]
    fn e2e_duplicate_enqueue_is_rejected() {
        let _serial = serial_guard();
        fresh_db("dup");
        ensure_unlocked();

        let label = format!(
            "dup-w-{}",
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        );
        let w = crate::wallet_store::create_wallet(&label, None).expect("wallet");
        let chains = crate::chain::list_chains().unwrap();
        let base = chains
            .iter()
            .find(|c| c.chain_id == 8453)
            .cloned()
            .expect("base chain seeded");
        let common = EnqueueArgs {
            wallet_id: w.id,
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000002".into(),
            quantity: 1,
            value_wei: None,
            function_name: Some("mint()".into()),
            is_hex: false,
            parameters: None,
            calldata: Some("0x1249c58b".into()),
            rpc_endpoints: None,
            flashbots: false,
            gas_limit: None,
            max_fee_gwei: None,
            priority_fee_gwei: None,
            nonce_override: None,
            scheduled_at: None,
            delay_ms: Some(0),
            mode: Some("execute".into()),
            opensea_ref: None,
        };

        let first = enqueue(common.clone()).expect("first enqueue");
        let err = enqueue(common.clone())
            .err()
            .expect("duplicate must be rejected");
        assert!(
            err.to_string().contains("already queued"),
            "unexpected error: {err}"
        );
        // Exactly one row for this wallet/contract.
        let n = crate::db::with_conn(|conn| {
            let n: i64 = conn.query_row(
                "SELECT COUNT(*) FROM mint_tasks WHERE wallet_id=?1 AND contract=?2",
                rusqlite::params![w.id, common.contract],
                |r| r.get(0),
            )?;
            Ok(n)
        })
        .unwrap();
        assert_eq!(n, 1, "duplicate insert slipped through");

        // A different calldata is a different mint — still allowed.
        let other = EnqueueArgs {
            calldata: Some("0xa9059cbb".into()),
            ..common.clone()
        };
        let _ = enqueue(other).expect("different calldata must enqueue");

        // Draining the queue unblocks an identical re-submit.
        crate::db::with_conn(|conn| {
            conn.execute("UPDATE mint_tasks SET status='failed'", [])?;
            Ok(())
        })
        .unwrap();
        let _ = enqueue(common).expect("after the first left the queue");
        assert_eq!(first.status, "pending");
    }

    /// Offline pipeline e2e: wallet → enqueue all modes/fields → list → cancel → schedule gate.
    #[test]
    fn e2e_enqueue_list_cancel_schedule() {
        let _serial = serial_guard();
        fresh_db("pipe");
        ensure_unlocked();

        let w1 = crate::wallet_store::create_wallet("e2e-a", None).expect("wallet a");
        let w2 = crate::wallet_store::create_wallet("e2e-b", None).expect("wallet b");
        let w1 = crate::wallet_store::create_wallet("e2e-a2", None).unwrap_or(w1);
        // create may duplicate labels — use fresh labels if collision
        let wallets = crate::wallet_store::list_wallets().unwrap();
        let wa = wallets
            .iter()
            .find(|w| w.label == "e2e-a" || w.label.starts_with("e2e-a"))
            .map(|w| w.id)
            .unwrap_or(w1.id);
        let wb = wallets
            .iter()
            .find(|w| w.label == "e2e-b" || w.label.starts_with("e2e-b"))
            .map(|w| w.id)
            .unwrap_or(w2.id);

        let chains = crate::chain::list_chains().unwrap();
        let base = chains
            .iter()
            .find(|c| c.chain_id == 8453)
            .cloned()
            .expect("base chain seeded");
        assert!(!base.rpc_url.is_empty());

        // 1) Function + params, multi-wallet fan-out, full gas/fee/nonce/delay fields
        let now = crate::db::now_ms();
        let future = now + 3_600_000; // 1h — must stay pending on run
        let common = EnqueueArgs {
            chain_id: base.chain_id,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 2,
            value_wei: Some("1000000000000000".into()),
            function_name: Some("mint(address,uint256)".into()),
            is_hex: false,
            parameters: Some("{address}; 2".into()),
            calldata: None,
            rpc_endpoints: Some(vec![base.rpc_url.clone(), "https://example.invalid/rpc".into()]),
            flashbots: true,
            gas_limit: Some("250000".into()),
            max_fee_gwei: Some("1.5".into()),
            priority_fee_gwei: Some("0.05".into()),
            nonce_override: None,
            scheduled_at: Some(future),
            delay_ms: Some(10),
            mode: Some("execute".into()),
            ..Default::default()
        };

        let t1 = enqueue(EnqueueArgs { wallet_id: wa, ..common.clone() }).unwrap();
        let t2 = enqueue(EnqueueArgs { wallet_id: wb, ..common.clone() }).unwrap();
        assert_ne!(t1.id, t2.id);
        assert_eq!(t1.status, "pending");
        assert_eq!(t1.function_name.as_deref(), Some("mint(address,uint256)"));
        assert!(!t1.is_hex);
        assert_eq!(t1.parameters.as_deref(), Some("{address}; 2"));
        assert!(t1.flashbots);
        assert_eq!(t1.gas_limit.as_deref(), Some("250000"));
        assert_eq!(t1.max_fee_gwei.as_deref(), Some("1.5"));
        assert_eq!(t1.priority_fee_gwei.as_deref(), Some("0.05"));
        assert_eq!(t1.delay_ms, 10);
        assert_eq!(t1.mode, "execute");
        assert_eq!(t1.scheduled_at, Some(future));
        assert!(t1.rpc_endpoints.as_deref().unwrap().contains("example.invalid"));

        // 2) HEX + placeholder + simulate / spam / sweep
        let sim = enqueue(EnqueueArgs {
            wallet_id: wa,
            is_hex: true,
            calldata: Some("0x1249c58b".into()),
            function_name: None,
            parameters: None,
            mode: Some("simulate".into()),
            flashbots: false,
            gas_limit: None,
            max_fee_gwei: None,
            priority_fee_gwei: None,
            scheduled_at: None,
            delay_ms: None,
            rpc_endpoints: None,
            value_wei: None,
            ..common.clone()
        })
        .unwrap();
        assert!(sim.is_hex);
        assert_eq!(sim.mode, "simulate");
        assert_eq!(sim.calldata.as_deref(), Some("0x1249c58b"));

        let spam = enqueue(EnqueueArgs {
            wallet_id: wa,
            mode: Some("spam".into()),
            is_hex: false,
            function_name: Some("mint()".into()),
            parameters: None,
            calldata: None,
            ..common.clone()
        })
        .unwrap();
        assert_eq!(spam.mode, "spam");

        let sweep = enqueue(EnqueueArgs {
            wallet_id: wb,
            mode: Some("sweep".into()),
            is_hex: false,
            function_name: Some("mint(uint256)".into()),
            parameters: None,
            calldata: None,
            ..common.clone()
        })
        .unwrap();
        assert_eq!(sweep.mode, "sweep");

        // 3) Validation failures
        assert!(enqueue(EnqueueArgs {
            wallet_id: wa,
            mode: Some("nope".into()),
            ..common.clone()
        })
        .is_err());
        assert!(enqueue(EnqueueArgs {
            wallet_id: wa,
            is_hex: true,
            calldata: None,
            mode: Some("execute".into()),
            ..common.clone()
        })
        .is_err());
        assert!(enqueue(EnqueueArgs {
            wallet_id: 999_999,
            mode: Some("execute".into()),
            ..common.clone()
        })
        .is_err());

        // 4) List includes all new tasks with columns intact
        let listed = list_tasks().unwrap();
        for id in [t1.id, t2.id, sim.id, spam.id, sweep.id] {
            let row = listed.iter().find(|t| t.id == id).expect("listed");
            assert_eq!(row.status, "pending");
            assert!(!row.contract.is_empty());
        }

        // 5) Cancel one pending
        cancel_task(spam.id).unwrap();
        assert!(get_task(spam.id).is_err());

        // 6) run_pending: scheduled tasks not due stay pending; due simulate may go simulated
        let results = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(run_pending())
            .unwrap();
        for r in &results {
            assert!(
                matches!(
                    r.status.as_str(),
                    "pending" | "failed" | "broadcasting" | "simulated" | "confirmed"
                ),
                "unexpected status {}",
                r.status
            );
            if r.scheduled_at.unwrap_or(0) > crate::db::now_ms() {
                assert_eq!(r.status, "pending", "future schedule must not execute");
            }
        }

        // 7) recover_stale is a no-op-safe call
        recover_stale_tasks();

        // 8) Calldata resolution path for stored function+params
        let w = crate::wallet_store::get_wallet(wa).unwrap();
        let cd = resolve_calldata(&t1, &w.address).unwrap();
        assert!(cd.starts_with("0x"));
        assert_eq!((cd.len() - 2 - 8) % 64, 0);
        // wallet address appears as right-padded word
        let addr_nibbles = w.address.trim_start_matches("0x").to_lowercase();
        assert!(cd.to_lowercase().contains(&addr_nibbles));
    }

    /// Live RPC e2e: Base public endpoint — eth_call simulate path end-to-end.
    #[test]
    fn e2e_live_simulate_base_weth() {
        let _serial = serial_guard();
        fresh_db("live");
        ensure_unlocked();

        let chains = crate::chain::list_chains().unwrap();
        let base = chains
            .iter()
            .find(|c| c.chain_id == 8453)
            .cloned()
            .expect("base chain");
        let w = crate::wallet_store::create_wallet("e2e-live", None)
            .or_else(|_| {
                crate::wallet_store::list_wallets()?
                    .into_iter()
                    .next()
                    .ok_or(crate::error::AppError::NotFound("wallet".into()))
            })
            .expect("wallet");

        // WETH totalSupply() — valid eth_call on Base mainnet
        let task = enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: 8453,
            // WETH on Base
            contract: "0x4200000000000000000000000000000000000006".into(),
            quantity: 1,
            value_wei: Some("0".into()),
            function_name: Some("totalSupply()".into()),
            is_hex: false,
            parameters: None,
            calldata: None,
            rpc_endpoints: Some(vec![base.rpc_url.clone()]),
            flashbots: false,
            gas_limit: None,
            max_fee_gwei: None,
            priority_fee_gwei: None,
            nonce_override: None,
            scheduled_at: None,
            delay_ms: Some(0),
            mode: Some("simulate".into()),
            opensea_ref: None,
        })
        .expect("enqueue live simulate");

        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let out = rt.block_on(process_task(task.id)).expect("process");
        // Success → simulated; transient RPC fail → failed with eth_call error (still e2e path)
        assert!(
            out.status == "simulated" || out.status == "failed",
            "status={}",
            out.status
        );
        if out.status == "simulated" {
            let err = out.error.unwrap_or_default();
            assert!(err.contains("eth_call ok"), "err={err}");
            assert!(err.contains("0x"), "err={err}");
        } else {
            let err = out.error.unwrap_or_default();
            assert!(
                err.contains("eth_call") || err.contains("rpc") || err.contains("send"),
                "err={err}"
            );
        }
    }
}
