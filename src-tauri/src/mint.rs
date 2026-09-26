use crate::chain;
use crate::error::{AppError, AppResult};
use crate::wallet_store;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

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
}

static MINT_RUNNING: AtomicBool = AtomicBool::new(false);
/// Task ids with a background receipt poll in flight — prevents duplicate
/// pollers across scheduler ticks and lets polls outlive the run guard.
static POLL_IN_FLIGHT: once_cell::sync::Lazy<std::sync::Mutex<std::collections::HashSet<i64>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

struct MintRunGuard;
impl Drop for MintRunGuard {
    fn drop(&mut self) {
        MINT_RUNNING.store(false, Ordering::SeqCst);
    }
}

/// Kick off a background receipt poll for `id` unless one is already running.
/// Detaching keeps the 30s poll window from holding `MINT_RUNNING` hostage —
/// fresh scheduled mints must be able to fire on the next 250ms tick.
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

pub fn is_running() -> bool {
    MINT_RUNNING.load(Ordering::SeqCst)
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

const SELECT_COLS: &str = "id, chain_id, contract, quantity, value_wei, calldata, status, tx_hash, error, wallet_id, created_at, updated_at, function_name, is_hex, parameters, rpc_endpoints, flashbots, gas_limit, max_fee_gwei, priority_fee_gwei, nonce_override, scheduled_at, delay_ms, mode, poll_attempts, auto_retries";

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
    })
}

pub fn list_tasks() -> AppResult<Vec<MintTaskRow>> {
    crate::db::with_conn(|conn| {
        let sql = format!("SELECT {SELECT_COLS} FROM mint_tasks ORDER BY created_at DESC");
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

/// Transient errors worth one automatic requeue (never reverts/param errors).
fn is_transient_err(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    const TRANSIENT: [&str; 14] = [
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
    ];
    const PERMANENT: [&str; 6] = [
        "revert",
        "insufficient funds",
        "chain mismatch",
        "bad gas",
        "bad max_fee",
        "bad value",
    ];
    if PERMANENT.iter().any(|p| m.contains(p)) {
        return false;
    }
    TRANSIENT.iter().any(|t| m.contains(t))
}

pub fn enqueue(a: EnqueueArgs) -> AppResult<MintTaskRow> {
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
        conn.execute(
            "INSERT INTO mint_tasks(
                chain_id, contract, quantity, value_wei, calldata, status, wallet_id, created_at, updated_at,
                function_name, is_hex, parameters, rpc_endpoints, flashbots,
                gas_limit, max_fee_gwei, priority_fee_gwei, nonce_override,
                scheduled_at, delay_ms, mode
             )
             VALUES (?1,?2,?3,?4,?5,'pending',?6,?7,?7, ?8,?9,?10,?11,?12, ?13,?14,?15,?16, ?17,?18,?19)",
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
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })?;

    wallet_store::log_activity(
        "mint.enqueue",
        &format!(
            "Queued mint #{id} → {c} x{} on chain {} ({mode})",
            a.quantity, a.chain_id
        ),
        None,
        true,
    );
    get_task(id)
}

pub fn cancel_task(id: i64) -> AppResult<()> {
    let n = crate::db::with_conn(|conn| {
        Ok(conn.execute(
            "DELETE FROM mint_tasks WHERE id = ?1 AND status = 'pending'",
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
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE mint_tasks SET status=?1, tx_hash=COALESCE(?2, tx_hash), error=?3, updated_at=?4 WHERE id=?5",
            rusqlite::params![status, tx_hash, error, now, id],
        )?;
        Ok(())
    });
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
            set_status(
                id,
                "failed",
                task.tx_hash.as_deref(),
                Some("no receipt after ~20m — tx dropped or RPC never saw it; re-enqueue if needed"),
            );
            return get_task(id);
        }
        if let Some(hash) = task.tx_hash.clone() {
            return poll_receipt(id, &hash).await;
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
        // Transient transport/RPC errors get one immediate requeue so a single
        // blip doesn't kill a scheduled FCFS task. Reverts/bad params still fail.
        if task.auto_retries < 1 && is_transient_err(&msg) {
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
        set_status(id, "failed", None, Some(&msg));
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

    let calldata = match resolve_calldata(&task, &wallet.address) {
        Ok(c) => c,
        Err(e) => return fail(format!("calldata: {e}")),
    };
    let value_wei = task.value_wei.clone().unwrap_or_else(|| "0x0".into());
    let value_u = match parse_u128_hex_or_dec(&value_wei) {
        Ok(v) => v,
        Err(e) => return fail(format!("{e}")),
    };

    let endpoints = parse_rpc_list(&task, &chain_row.rpc_url);
    let primary = endpoints[0].clone();

    let tx = serde_json::json!({
        "from": wallet.address,
        "to": task.contract,
        "data": calldata,
        "value": format!("0x{:x}", value_u),
    });

    // Simulate mode: eth_call only, no sign/broadcast.
    // Chain-id check runs concurrently so a wrong-chain RPC fails fast instead
    // of returning a misleading result from another network.
    if task.mode == "simulate" {
        let (chk, call_result) = tokio::join!(
            chain::eth_chain_id(&primary),
            chain::rpc_call(&primary, "eth_call", serde_json::json!([tx, "latest"]))
        );
        if let Ok(cid) = chk {
            if cid != task.chain_id {
                return fail(format!(
                    "RPC chain mismatch: endpoint is chain {cid}, task expects {}",
                    task.chain_id
                ));
            }
        }
        return match call_result {
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
                    &format!("Mint #{id} simulated ok"),
                    None,
                    true,
                );
                get_task(id)
            }
            Err(e) => fail(format!("eth_call: {e}")),
        };
    }

    // Verify the primary endpoint's chain_id concurrently with the nonce fetch —
    // a mismatched RPC would otherwise sign a valid tx for the wrong network.
    let nonce_task = task.nonce_override.clone();
    let primary_for_nonce = primary.clone();
    let addr_for_nonce = wallet.address.clone();
    let (chk, nonce_res) = tokio::join!(
        chain::eth_chain_id(&primary),
        async move {
            match &nonce_task {
                Some(n) => n
                    .parse::<u64>()
                    .map_err(|_| AppError::Invalid(format!("bad nonce override: {n}"))),
                None => {
                    chain::get_transaction_count(&primary_for_nonce, &addr_for_nonce).await
                }
            }
        }
    );
    if let Ok(cid) = chk {
        if cid != task.chain_id {
            return fail(format!(
                "RPC chain mismatch: endpoint is chain {cid}, task expects {}",
                task.chain_id
            ));
        }
    }
    let nonce = match nonce_res {
        Ok(n) => n,
        Err(e) => return fail(format!("nonce: {e}")),
    };

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
        match chain::estimate_gas(&primary, tx.clone()).await {
            Ok(g) => g.saturating_mul(115) / 100,
            Err(e) => {
                // A revert means the mint would burn gas and fail on-chain —
                // surface it instead of broadcasting a doomed tx.
                if e.to_string().to_lowercase().contains("revert") {
                    return fail(format!("estimate_gas reverted (check params/value/timing): {e}"));
                }
                // Some RPCs (e.g. Robinhood) omit/fail eth_estimateGas — use a
                // safe default for mint calldata rather than blocking the run.
                300_000
            }
        }
    };

    let (max_fee, tip) = if task.max_fee_gwei.is_some() || task.priority_fee_gwei.is_some() {
        let max_fee = match &task.max_fee_gwei {
            Some(s) => match s.parse::<f64>() {
                Ok(g) => (g * 1e9) as u128,
                Err(_) => return fail(format!("bad max_fee_gwei: {s}")),
            },
            None => match chain::gas_price(&primary).await {
                Ok(p) => p.saturating_mul(110) / 100,
                Err(e) => return fail(format!("gasPrice: {e}")),
            },
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
        let gas_price = match chain::gas_price(&primary).await {
            Ok(p) => p.saturating_mul(110) / 100,
            Err(e) => return fail(format!("gasPrice: {e}")),
        };
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
            to: &task.contract,
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
        _ => vec![primary],
    };

    let mut last_err: Option<String> = None;
    let mut first_hash: Option<String> = None;

    for (i, url) in send_targets.iter().enumerate() {
        if task.mode == "sweep" && first_hash.is_some() {
            // Sweep stops at first success (sequential fallback).
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
                        &format!("Mint #{id} broadcast {hash} via {url}"),
                        None,
                        true,
                    );
                } else {
                    wallet_store::log_activity(
                        "mint.broadcast",
                        &format!("Mint #{id} also sent via {url}"),
                        None,
                        true,
                    );
                }
            }
            Err(e) => {
                last_err = Some(format!("{url}: {e}"));
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
    let poll_url = parse_rpc_list(&task, &chain_row.rpc_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| chain_row.rpc_url.clone());
    let mut consecutive_errs = 0u32;
    for _ in 0..15 {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        match chain::get_receipt(&poll_url, hash).await {
            Ok(Some(receipt)) => {
                let status = receipt
                    .get("status")
                    .and_then(|s| s.as_str())
                    .unwrap_or("0x1");
                if status == "0x1" {
                    set_status(id, "confirmed", Some(hash), None);
                    wallet_store::log_activity(
                        "mint.confirmed",
                        &format!("Mint #{id} confirmed {hash}"),
                        None,
                        true,
                    );
                } else {
                    set_status(id, "failed", Some(hash), Some("reverted on-chain"));
                    wallet_store::log_activity(
                        "mint.failed",
                        &format!("Mint #{id} reverted {hash}"),
                        None,
                        false,
                    );
                }
                return get_task(id);
            }
            Ok(None) => {
                consecutive_errs = 0;
                continue;
            }
            Err(e) => {
                // Transient RPC hiccups must not abort the whole poll window —
                // only give up after several consecutive failures.
                consecutive_errs += 1;
                if consecutive_errs >= 5 {
                    set_status(
                        id,
                        "broadcasting",
                        Some(hash),
                        Some(&format!("receipt retry: {e}")),
                    );
                    return get_task(id);
                }
            }
        }
    }
    set_status(
        id,
        "broadcasting",
        Some(hash),
        Some("timeout waiting for receipt — will re-poll on next run"),
    );
    get_task(id)
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

/// Kick worker: process pending tasks under the run guard, then hand receipt
/// re-polls to background tasks so slow polls never block the next tick.
/// Tasks are grouped by wallet: different wallets run in parallel (FCFS race),
/// same-wallet tasks stay sequential to avoid nonce collisions.
pub async fn run_pending() -> AppResult<Vec<MintTaskRow>> {
    if MINT_RUNNING.swap(true, Ordering::SeqCst) {
        return Err(AppError::Other("mint run already in progress".into()));
    }
    let _guard = MintRunGuard;
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
    let mut results = Vec::new();
    // Group pending tasks by wallet (None → single shared bucket).
    let mut by_wallet: std::collections::HashMap<Option<i64>, Vec<i64>> =
        std::collections::HashMap::new();
    for (id, wallet_id) in pending {
        by_wallet.entry(wallet_id).or_default().push(id);
    }
    // Spawn one sequential lane per wallet so distinct wallets fire together.
    let mut lanes = Vec::new();
    for (_wallet_id, ids) in by_wallet {
        lanes.push(tokio::spawn(async move {
            let mut out = Vec::new();
            for id in ids {
                match process_task(id).await {
                    Ok(t) => out.push(t),
                    Err(e) => {
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
    for lane in lanes {
        if let Ok(mut batch) = lane.await {
            results.append(&mut batch);
        }
    }
    // Resume receipt re-polls in the background (skipped when already in
    // flight) so this run returns immediately and frees the run guard.
    for id in broadcasting {
        if let Ok(t) = get_task(id) {
            results.push(t);
        }
        spawn_receipt_poll(id);
    }
    Ok(results)
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
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
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
