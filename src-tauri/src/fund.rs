use crate::chain::{self, erc20};
use crate::error::{AppError, AppResult};
use crate::mint::{sign_eip1559, SignParams};
use crate::wallet_store;
use alloy::primitives::U256;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};

static FUND_RUNNING: AtomicBool = AtomicBool::new(false);

/// Clear `FUND_RUNNING` when the owning runner exits (success or panic).
struct RunGuard;
impl Drop for RunGuard {
    fn drop(&mut self) {
        FUND_RUNNING.store(false, Ordering::SeqCst);
    }
}

fn try_claim_runner() -> bool {
    FUND_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

fn release_runner() {
    FUND_RUNNING.store(false, Ordering::SeqCst);
}

#[derive(Serialize, Clone)]
pub struct FundJobRow {
    pub id: i64,
    pub mode: String,
    pub amount_mode: String,
    pub asset: String,
    pub chain_id: i64,
    pub amount_wei: String,
    pub anchor_wallet_id: i64,
    pub peer_wallet_ids: Vec<i64>,
    pub status: String,
    pub total_count: i64,
    pub success_count: i64,
    pub failed_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Serialize, Clone)]
pub struct FundTxRow {
    pub id: i64,
    pub job_id: i64,
    pub seq: i64,
    pub kind: String,
    pub wallet_id: i64,
    pub from_address: String,
    pub to_address: String,
    pub amount_wei: String,
    pub status: String,
    pub tx_hash: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Receipt-poll attempts accumulated across runner re-entries.
    pub poll_attempts: i64,
}

#[derive(Clone)]
pub struct FundStartArgs {
    pub mode: String,
    pub amount_mode: String,
    pub asset: String,
    pub chain_id: i64,
    pub amount_wei: String,
    pub anchor_wallet_id: i64,
    pub peer_wallet_ids: Vec<i64>,
}

#[derive(Serialize)]
pub struct FundPreviewRow {
    pub wallet_id: i64,
    pub from_address: String,
    pub to_address: String,
    pub balance_wei: String,
    pub send_wei: String,
    pub skip_reason: Option<String>,
}

#[derive(Serialize)]
pub struct FundPreview {
    pub rows: Vec<FundPreviewRow>,
    pub total_send_wei: String,
    pub is_native: bool,
    pub decimals: u8,
    pub symbol: String,
    pub warnings: Vec<String>,
}

const JOB_COLS: &str =
    "id, mode, amount_mode, asset, chain_id, amount_wei, anchor_wallet_id, peer_wallet_ids, status, total_count, success_count, failed_count, created_at, updated_at";
const TX_COLS: &str =
    "id, job_id, seq, kind, wallet_id, from_address, to_address, amount_wei, status, tx_hash, error, created_at, updated_at, poll_attempts";

fn validate_mode_args(mode: &str, amount_mode: &str) -> AppResult<()> {
    if mode != "disperse" && mode != "consolidate" {
        return Err(AppError::Invalid(
            "mode must be disperse|consolidate".into(),
        ));
    }
    if amount_mode != "fixed" && amount_mode != "target" {
        return Err(AppError::Invalid(
            "amount_mode must be fixed|target".into(),
        ));
    }
    Ok(())
}

fn parse_u256(s: &str) -> AppResult<U256> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x") {
        U256::from_str_radix(h, 16).map_err(|_| AppError::Invalid(format!("bad hex amount: {s}")))
    } else {
        t.parse::<U256>()
            .map_err(|_| AppError::Invalid(format!("bad amount: {s}")))
    }
}

fn is_valid_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn short_addr(a: &str) -> String {
    if a.len() >= 10 {
        format!("{}…{}", &a[..6], &a[a.len() - 4..])
    } else {
        a.to_string()
    }
}

/// Pure amount math shared by preview and the runner.
/// `Err(reason)` marks the transfer as skippable (already at target / dust).
pub fn calc_send_wei(
    amount_mode: &str,
    kind: &str,
    configured: U256,
    sender_token_bal: U256,
    dest_token_bal: U256,
    gas_cost_native: U256,
    is_native: bool,
) -> Result<U256, &'static str> {
    match amount_mode {
        "fixed" => {
            if configured == 0 {
                return Err("amount is zero");
            }
            if sender_token_bal < configured {
                return Err("insufficient balance");
            }
            // Native fixed: sender must also cover gas on top of the send.
            if is_native && gas_cost_native > 0 && sender_token_bal < configured + gas_cost_native
            {
                return Err("insufficient for amount + gas");
            }
            Ok(configured)
        }
        "target" => match kind {
            "disperse" => {
                if dest_token_bal >= configured {
                    return Err("already at target");
                }
                let send = configured - dest_token_bal;
                if send == 0 {
                    return Err("already at target");
                }
                if sender_token_bal < send {
                    return Err("insufficient balance");
                }
                Ok(send)
            }
            "consolidate" => {
                if sender_token_bal <= configured {
                    return Err("below target");
                }
                let mut send = sender_token_bal - configured;
                if is_native {
                    if send <= gas_cost_native {
                        return Err("dust after gas");
                    }
                    send -= gas_cost_native;
                }
                Ok(send)
            }
            _ => Err("bad kind"),
        },
        _ => Err("bad amount mode"),
    }
}

fn map_job(r: &rusqlite::Row) -> rusqlite::Result<FundJobRow> {
    let peers_json: String = r.get(7)?;
    let peer_wallet_ids: Vec<i64> = serde_json::from_str(&peers_json).unwrap_or_default();
    Ok(FundJobRow {
        id: r.get(0)?,
        mode: r.get(1)?,
        amount_mode: r.get(2)?,
        asset: r.get(3)?,
        chain_id: r.get(4)?,
        amount_wei: r.get(5)?,
        anchor_wallet_id: r.get(6)?,
        peer_wallet_ids,
        status: r.get(8)?,
        total_count: r.get(9)?,
        success_count: r.get(10)?,
        failed_count: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

fn map_tx(r: &rusqlite::Row) -> rusqlite::Result<FundTxRow> {
    Ok(FundTxRow {
        id: r.get(0)?,
        job_id: r.get(1)?,
        seq: r.get(2)?,
        kind: r.get(3)?,
        wallet_id: r.get(4)?,
        from_address: r.get(5)?,
        to_address: r.get(6)?,
        amount_wei: r.get(7)?,
        status: r.get(8)?,
        tx_hash: r.get(9)?,
        error: r.get(10)?,
        created_at: r.get(11)?,
        updated_at: r.get(12)?,
        poll_attempts: r.get(13)?,
    })
}

pub fn get_job(id: i64) -> AppResult<FundJobRow> {
    crate::db::with_conn(|conn| {
        let sql = format!("SELECT {JOB_COLS} FROM fund_jobs WHERE id = ?1");
        let mut stmt = conn.prepare(&sql)?;
        stmt.query_row([id], map_job).map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                AppError::NotFound(format!("fund job {id}"))
            }
            other => AppError::Db(other),
        })
    })
}

pub fn list_job_txs(job_id: i64) -> AppResult<Vec<FundTxRow>> {
    crate::db::with_conn(|conn| {
        let sql = format!(
            "SELECT {TX_COLS} FROM fund_txs WHERE job_id = ?1 ORDER BY seq ASC"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map([job_id], map_tx)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn active_job() -> AppResult<Option<FundJobRow>> {
    crate::db::with_conn(|conn| {
        let sql = format!(
            "SELECT {JOB_COLS} FROM fund_jobs WHERE status = 'running' ORDER BY id DESC LIMIT 1"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query_map([], map_job)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    })
}

fn refresh_counts(job_id: i64) {
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE fund_jobs SET
                success_count = (SELECT COUNT(*) FROM fund_txs WHERE job_id = ?1 AND status = 'confirmed'),
                failed_count = (SELECT COUNT(*) FROM fund_txs WHERE job_id = ?1 AND status IN ('failed','skipped')),
                updated_at = ?2
             WHERE id = ?1",
            rusqlite::params![job_id, crate::db::now_ms()],
        )?;
        Ok(())
    });
}

fn set_tx_status(
    tx_id: i64,
    status: &str,
    tx_hash: Option<&str>,
    error: Option<&str>,
    amount_wei: Option<&str>,
) {
    let now = crate::db::now_ms();
    // Same rule as mint_tasks.error: transport errors carry the endpoint URL,
    // and RPC URLs can hold API keys — redact before persisting.
    let error = error.map(crate::chain::redact_urls_in);
    let mut job_id = 0i64;
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE fund_txs SET
                status = ?1,
                tx_hash = COALESCE(?2, tx_hash),
                error = ?3,
                amount_wei = COALESCE(?4, amount_wei),
                updated_at = ?5
             WHERE id = ?6",
            rusqlite::params![status, tx_hash, error, amount_wei, now, tx_id],
        )?;
        job_id = conn
            .query_row("SELECT job_id FROM fund_txs WHERE id = ?1", [tx_id], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        Ok(())
    });
    if job_id > 0 {
        refresh_counts(job_id);
    }
}

fn validate_args(a: &FundStartArgs) -> AppResult<()> {
    if !crate::vault::is_unlocked()? {
        return Err(AppError::VaultLocked);
    }
    validate_mode_args(&a.mode, &a.amount_mode)?;
    if a.chain_id <= 0 {
        return Err(AppError::Invalid("chain_id required".into()));
    }
    if a.peer_wallet_ids.is_empty() {
        return Err(AppError::Invalid("select at least one peer wallet".into()));
    }
    if a.anchor_wallet_id <= 0 {
        return Err(AppError::Invalid("anchor wallet required".into()));
    }
    let mut uniq = std::collections::HashSet::new();
    for id in &a.peer_wallet_ids {
        if !uniq.insert(*id) {
            return Err(AppError::Invalid(format!(
                "duplicate peer wallet {id} in selection"
            )));
        }
    }
    if a.peer_wallet_ids.contains(&a.anchor_wallet_id) {
        return Err(AppError::Invalid(
            "anchor wallet must not appear in the peer list".into(),
        ));
    }
    let asset = a.asset.trim();
    if asset != "native" && !is_valid_address(asset) {
        return Err(AppError::Invalid(
            "asset must be 'native' or a 0x token address".into(),
        ));
    }
    if a.amount_wei.trim().is_empty() {
        return Err(AppError::Invalid("amount required".into()));
    }
    let amount = parse_u256(&a.amount_wei)?;
    if a.amount_mode == "fixed" && amount == 0 {
        return Err(AppError::Invalid("amount must be > 0".into()));
    }
    // Every wallet must exist.
    wallet_store::get_wallet(a.anchor_wallet_id)?;
    for id in &a.peer_wallet_ids {
        wallet_store::get_wallet(*id)?;
    }
    let chains = chain::list_chains()?;
    if !chains.iter().any(|c| c.chain_id == a.chain_id) {
        return Err(AppError::NotFound(format!("chain {}", a.chain_id)));
    }
    Ok(())
}

/// Build job + tx rows without spawning the runner (testable offline).
pub fn create_job(a: FundStartArgs) -> AppResult<FundJobRow> {
    validate_args(&a)?;
    let is_disperse = a.mode == "disperse";
    let now = crate::db::now_ms();
    let peers_json = serde_json::to_string(&a.peer_wallet_ids)
        .map_err(|_| AppError::Other("serialize peers".into()))?;

    let job_id = crate::db::with_conn(|conn| {
        conn.execute(
            "INSERT INTO fund_jobs(
                mode, amount_mode, asset, chain_id, amount_wei,
                anchor_wallet_id, peer_wallet_ids, status,
                total_count, success_count, failed_count, created_at, updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,'running',?8,0,0,?9,?9)",
            rusqlite::params![
                a.mode,
                a.amount_mode,
                a.asset.trim(),
                a.chain_id,
                a.amount_wei.trim(),
                a.anchor_wallet_id,
                peers_json,
                a.peer_wallet_ids.len() as i64,
                now,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })?;

    let anchor = wallet_store::get_wallet(a.anchor_wallet_id)?;
    let kind = if is_disperse { "disperse" } else { "consolidate" };
    // Fixed amounts are known up front so the progress table can show them
    // before broadcast; target amounts are filled in when the runner calculates.
    let initial_amount = if a.amount_mode == "fixed" {
        a.amount_wei.trim()
    } else {
        "0"
    };
    for (i, peer_id) in a.peer_wallet_ids.iter().enumerate() {
        let peer = wallet_store::get_wallet(*peer_id)?;
        let (from, to) = if is_disperse {
            (anchor.address.clone(), peer.address.clone())
        } else {
            (peer.address.clone(), anchor.address.clone())
        };
        let seq = i as i64 + 1;
        // disperse: anchor sends → anchor signs; consolidate: peer sends → peer signs
        let signer_wallet_id = if is_disperse { a.anchor_wallet_id } else { *peer_id };
        crate::db::with_conn(|conn| {
            conn.execute(
                "INSERT INTO fund_txs(
                    job_id, seq, kind, wallet_id, from_address, to_address,
                    amount_wei, status, created_at, updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?8)",
                rusqlite::params![
                    job_id,
                    seq,
                    kind,
                    signer_wallet_id,
                    from,
                    to,
                    initial_amount,
                    now
                ],
            )?;
            Ok(())
        })?;
    }

    wallet_store::log_activity(
        "fund.start",
        &format!(
            "Fund job #{job_id} {} {} x {} on chain {} ({} txs)",
            a.mode,
            a.amount_mode,
            a.amount_wei.trim(),
            a.chain_id,
            a.peer_wallet_ids.len()
        ),
        None,
        true,
    );
    get_job(job_id)
}

/// Create + spawn background runner. Called from the async Tauri command.
/// Claims the single-runner slot *before* create_job so two rapid starts
/// cannot both insert a job while only one runner ever exists.
pub fn start(a: FundStartArgs) -> AppResult<FundJobRow> {
    if !try_claim_runner() {
        return Err(AppError::Other(
            "a fund job is already running — stop it first".into(),
        ));
    }
    let job = match create_job(a) {
        Ok(j) => j,
        Err(e) => {
            release_runner();
            return Err(e);
        }
    };
    tokio::spawn(async move {
        let _guard = RunGuard;
        if let Err(e) = run_job_loop(job.id).await {
            wallet_store::log_activity(
                "fund.error",
                &format!("Fund job #{job_id} runner error: {e}", job_id = job.id),
                None,
                false,
            );
        }
    });
    Ok(job)
}

pub fn stop_job(job_id: i64) -> AppResult<FundJobRow> {
    let job = get_job(job_id)?;
    if job.status != "running" {
        return Err(AppError::Invalid(format!(
            "job {} is not running",
            job_id
        )));
    }
    let now = crate::db::now_ms();
    crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE fund_txs SET status = 'skipped', error = 'stopped by user', updated_at = ?1
             WHERE job_id = ?2 AND status = 'pending'",
            rusqlite::params![now, job_id],
        )?;
        conn.execute(
            "UPDATE fund_jobs SET status = 'stopped', updated_at = ?1 WHERE id = ?2",
            rusqlite::params![now, job_id],
        )?;
        Ok(())
    })?;
    refresh_counts(job_id);
    wallet_store::log_activity(
        "fund.stop",
        &format!("Stopped fund job #{job_id}"),
        None,
        true,
    );
    get_job(job_id)
}

/// Startup recovery: interrupted signing rows fail closed (no double-send);
/// jobs still marked running get their runner respawned to finish pending
/// work / re-poll broadcasts. Spawn is skipped outside a Tokio runtime
/// (unit tests) — the SQL recovery still runs.
pub fn recover_stale() {
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE fund_txs SET status = 'failed', error = 'interrupted by restart', updated_at = ?1
             WHERE status = 'signing'",
            [crate::db::now_ms()],
        )?;
        Ok(())
    });
    let running: Vec<i64> = crate::db::with_conn(|conn| {
        let mut stmt = conn.prepare("SELECT id FROM fund_jobs WHERE status = 'running'")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
    .unwrap_or_default();
    if tokio::runtime::Handle::try_current().is_err() {
        return; // no runtime (sync unit tests)
    }
    // Run recovered jobs one at a time under a single claim.
    if running.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for id in running {
            if !try_claim_runner() {
                break;
            }
            {
                let _guard = RunGuard;
                if let Err(e) = run_job_loop(id).await {
                    crate::logging::error(
                        "fund",
                        &format!("Fund job #{id} recover runner error: {e}"),
                    );
                    wallet_store::log_activity(
                        "fund.error",
                        &format!("Fund job #{id} recover runner error: {e}"),
                        None,
                        false,
                    );
                }
            }
        }
    });
}

async fn process_one_tx(tx: &FundTxRow, job: &FundJobRow) -> AppResult<()> {
    // Claim
    let claimed = crate::db::with_conn(|conn| {
        let n = conn.execute(
            "UPDATE fund_txs SET status = 'signing', updated_at = ?1
             WHERE id = ?2 AND status IN ('pending','broadcasting','confirming')",
            rusqlite::params![crate::db::now_ms(), tx.id],
        )?;
        Ok(n > 0)
    })?;
    if !claimed {
        return Ok(());
    }

    let fail = |msg: String| {
        let msg = chain::redact_urls_in(&msg);
        set_tx_status(tx.id, "failed", None, Some(&msg), None);
        crate::logging::error("fund", &format!("Fund tx #{} failed: {msg}", tx.id));
        wallet_store::log_activity(
            "fund.failed",
            &format!("Fund tx #{} failed: {msg}", tx.id),
            None,
            false,
        );
    };

    // Resume path: already have a hash — just poll the receipt.
    if let Some(hash) = tx.tx_hash.clone() {
        return poll_receipt(tx.id, &hash, job.chain_id).await;
    }

    let chains = chain::list_chains()?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == job.chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {}", job.chain_id)))?;

    // Single-call sites (balances, gas, nonce, send) use the chain's PRIMARY
    // endpoint — the raw rpc_url can be a comma-separated endpoint list, which
    // is only valid after endpoint_list() splits it. poll_receipt rotates.
    let primary = chain::endpoint_list(&chain_row.rpc_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| chain_row.rpc_url.clone());

    // Resolves the wallet row (existence check) before any signing work.
    if let Err(e) = wallet_store::get_wallet(tx.wallet_id) {
        fail(format!("wallet: {e}"));
        return Ok(());
    }
    let is_native = job.asset == "native";
    let configured = match parse_u256(&job.amount_wei) {
        Ok(v) => v,
        Err(e) => {
            fail(format!("amount: {e}"));
            return Ok(());
        }
    };

    // Balances
    let sender_native = match chain::native_balance(&primary, &tx.from_address).await {
        Ok(hex) => parse_u256(&hex).unwrap_or(U256::ZERO),
        Err(e) => {
            fail(format!("native balance: {e}"));
            return Ok(());
        }
    };
    let (sender_token_bal, dest_token_bal) = if is_native {
        // Disperse-target math needs the destination's real native balance.
        let dest_native =
            match chain::native_balance(&primary, &tx.to_address).await {
                Ok(hex) => parse_u256(&hex).unwrap_or(U256::ZERO),
                Err(_) => U256::ZERO,
            };
        (sender_native, dest_native)
    } else {
        let sb = erc20::balance_of(&primary, &job.asset, &tx.from_address)
            .await
            .unwrap_or(U256::ZERO);
        let db_ = erc20::balance_of(&primary, &job.asset, &tx.to_address)
            .await
            .unwrap_or(U256::ZERO);
        (sb, db_)
    };

    // Build tx payload for estimate
    let native_value_u128 = |v: U256| -> AppResult<u128> {
        u128::try_from(v).map_err(|_| AppError::Invalid("amount exceeds u128".into()))
    };

    let (tx_to, tx_data, value_for_estimate) = if is_native {
        let v = match calc_send_wei(
            &job.amount_mode,
            &tx.kind,
            configured,
            sender_token_bal,
            dest_token_bal,
            U256::ZERO,
            true,
        ) {
            Ok(v) => v,
            Err(reason) => {
                set_tx_status(tx.id, "skipped", None, Some(reason), None);
                return Ok(());
            }
        };
        (tx.to_address.clone(), "0x".to_string(), v)
    } else {
        let v = match calc_send_wei(
            &job.amount_mode,
            &tx.kind,
            configured,
            sender_token_bal,
            dest_token_bal,
            U256::ZERO,
            false,
        ) {
            Ok(v) => v,
            Err(reason) => {
                set_tx_status(tx.id, "skipped", None, Some(reason), None);
                return Ok(());
            }
        };
        let cd = match erc20::encode_transfer(&tx.to_address, v) {
            Ok(c) => c,
            Err(e) => {
                fail(format!("encode transfer: {e}"));
                return Ok(());
            }
        };
        (job.asset.clone(), cd, U256::ZERO)
    };

    let gas_price = match chain::gas_price(&primary).await {
        Ok(p) => p.saturating_mul(110) / 100,
        Err(e) => {
            fail(format!("gasPrice: {e}"));
            return Ok(());
        }
    };
    let tip = {
        let t = gas_price / 10;
        if t == 0 {
            gas_price.min(1)
        } else {
            t
        }
    };

    let estimate_payload = serde_json::json!({
        "from": tx.from_address,
        "to": tx_to,
        "data": if tx_data == "0x" { serde_json::json!("0x") } else { serde_json::json!(tx_data) },
        "value": format!("0x{:x}", value_for_estimate),
    });
    let gas = match chain::estimate_gas(&primary, estimate_payload).await {
        Ok(g) => g.saturating_mul(115) / 100,
        Err(_) => {
            if is_native {
                21_000
            } else {
                65_000
            }
        }
    };

    let gas_cost_native = U256::from(gas) * U256::from(gas_price);
    // Recompute amount now that gas is known (consolidate-target native).
    let send_amount = if is_native {
        match calc_send_wei(
            &job.amount_mode,
            &tx.kind,
            configured,
            sender_token_bal,
            dest_token_bal,
            gas_cost_native,
            true,
        ) {
            Ok(v) => v,
            Err(reason) => {
                set_tx_status(tx.id, "skipped", None, Some(reason), None);
                return Ok(());
            }
        }
    } else {
        match calc_send_wei(
            &job.amount_mode,
            &tx.kind,
            configured,
            sender_token_bal,
            dest_token_bal,
            gas_cost_native,
            false,
        ) {
            Ok(v) => v,
            Err(reason) => {
                set_tx_status(tx.id, "skipped", None, Some(reason), None);
                return Ok(());
            }
        }
    };

    // Final balance gates
    let need_native = if is_native {
        send_amount + gas_cost_native
    } else {
        gas_cost_native
    };
    if sender_native < need_native {
        fail(format!(
            "insufficient native balance for {}",
            if is_native { "amount + gas" } else { "gas" }
        ));
        return Ok(());
    }
    if is_native && sender_token_bal < send_amount {
        fail("insufficient balance".into());
        return Ok(());
    }

    // Persist the calculated amount so the UI can show it while signing.
    set_tx_status(tx.id, "signing", None, None, Some(&send_amount.to_string()));

    let value_u = if is_native {
        match native_value_u128(send_amount) {
            Ok(v) => v,
            Err(e) => {
                fail(format!("{e}"));
                return Ok(());
            }
        }
    } else {
        0
    };

    let chain_nonce = match chain::get_transaction_count(&primary, &tx.from_address).await
    {
        Ok(n) => n,
        Err(e) => {
            fail(format!("nonce: {e}"));
            return Ok(());
        }
    };
    // Mint lanes sign from the same wallet: one allocator keeps both from
    // handing out the same nonce after reading the chain at the same moment.
    let nonce = crate::nonce::reserve(tx.wallet_id, job.chain_id, chain_nonce, None);

    let signer = match (|| -> AppResult<_> {
        let (nonce_bytes, ct) = wallet_store::key_material(tx.wallet_id)?;
        crate::wallet::load_signer(&nonce_bytes, &ct)
    })() {
        Ok(s) => s,
        Err(e) => {
            crate::nonce::release(tx.wallet_id, job.chain_id, nonce);
            fail(format!("load signer: {e}"));
            return Ok(());
        }
    };

    let signed = match sign_eip1559(
        &signer,
        SignParams {
            to: &tx_to,
            data_hex: &tx_data,
            value: value_u,
            gas_limit: gas,
            nonce,
            chain_id: job.chain_id as u64,
            max_fee: gas_price,
            tip,
        },
    ) {
        Ok(s) => s,
        Err(e) => {
            crate::nonce::release(tx.wallet_id, job.chain_id, nonce);
            fail(format!("sign: {e}"));
            return Ok(());
        }
    };

    let local_hash = {
        let raw = match hex::decode(signed.trim_start_matches("0x")) {
            Ok(r) => r,
            Err(_) => {
                crate::nonce::release(tx.wallet_id, job.chain_id, nonce);
                fail("internal: signed tx not hex".into());
                return Ok(());
            }
        };
        format!("0x{}", hex::encode(alloy::primitives::keccak256(&raw)))
    };
    set_tx_status(tx.id, "broadcasting", Some(&local_hash), None, Some(&send_amount.to_string()));
    // Remember the reserved nonce so the receipt poll can hand it back when
    // the tx proves to be a phantom (stamped pre-send, never accepted).
    let _ = crate::db::with_conn(|conn| {
        conn.execute(
            "UPDATE fund_txs SET tx_nonce=?1 WHERE id=?2",
            rusqlite::params![nonce as i64, tx.id],
        )?;
        Ok(())
    });

    match chain::send_raw_transaction(&primary, &signed).await {
        Ok(hash) => {
            set_tx_status(tx.id, "confirming", Some(&hash), None, None);
            wallet_store::log_activity(
                "fund.broadcast",
                &format!("Fund tx #{} broadcast {hash}", tx.id),
                None,
                true,
            );
            poll_receipt(tx.id, &hash, job.chain_id).await
        }
        Err(e) => {
            if crate::mint::is_transient_err(&format!("send: {e}")) {
                // A timeout or 429 does not prove the node dropped it — it may
                // already hold the tx. Poll the local hash; re-signing here is
                // what would send the amount twice.
                set_tx_status(tx.id, "confirming", Some(&local_hash), None, None);
                crate::logging::warn(
                    "fund",
                    &format!("Fund tx #{} send ambiguous, polling local hash: {e}", tx.id),
                );
                wallet_store::log_activity(
                    "fund.retry",
                    &format!(
                        "Fund tx #{} send ambiguous ({e}) — polling instead of re-sending",
                        tx.id
                    ),
                    None,
                    true,
                );
                return poll_receipt(tx.id, &local_hash, job.chain_id).await;
            }
            // Definite rejection: this nonce never entered a mempool.
            crate::nonce::release(tx.wallet_id, job.chain_id, nonce);
            // Keep the local hash for audit but mark failed.
            set_tx_status(
                tx.id,
                "failed",
                Some(&local_hash),
                Some(&format!("send: {e}")),
                None,
            );
            crate::logging::error("fund", &format!("Fund tx #{} send failed: {e}", tx.id));
            wallet_store::log_activity(
                "fund.failed",
                &format!("Fund tx #{} send failed: {e}", tx.id),
                None,
                false,
            );
            Ok(())
        }
    }
}

/// Receipt polling budget per tx: 2s apart, so ~5 minutes total. The counter
/// lives on the row because the runner re-enters the poll after every give-up.
const FUND_POLL_MAX_ATTEMPTS: i64 = 150;

/// Hand a fund tx's reserved nonce back — called when the receipt poll proves
/// the tx never entered any mempool. Mirrors mint's release_task_nonce.
pub(crate) fn release_fund_tx_nonce(tx_id: i64) {
    let row: Option<(i64, Option<i64>, i64)> = crate::db::with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT t.wallet_id, t.tx_nonce, j.chain_id
                 FROM fund_txs t JOIN fund_jobs j ON j.id = t.job_id
                 WHERE t.id = ?1",
                [tx_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok())
    })
    .ok()
    .flatten();
    if let Some((wallet_id, Some(nonce), chain_id)) = row {
        crate::nonce::release(wallet_id, chain_id, nonce as u64);
        crate::logging::info(
            "fund",
            &format!("Fund tx #{tx_id}: released unused nonce {nonce}"),
        );
    }
}

/// True when at least one right-chain endpoint still knows `hash` — receipt
/// mined or sitting in a mempool. Drives both the fast phantom check (~10s)
/// and the over-budget "keep watching" decision (double-send guard).
async fn tx_known_anywhere(hash: &str, chain_id: i64, endpoints: &[String]) -> bool {
    const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);
    for url in endpoints {
        let cid = match tokio::time::timeout(PROBE_TIMEOUT, chain::eth_chain_id(url)).await {
            Ok(Ok(cid)) => Some(cid),
            _ => None,
        };
        let Some(cid) = cid else { continue };
        if cid != chain_id {
            continue;
        }
        let receipt =
            match tokio::time::timeout(PROBE_TIMEOUT, chain::get_receipt(url, hash)).await {
                Ok(Ok(r)) => r,
                _ => None,
            };
        if receipt.is_some() {
            return true;
        }
        let known = match tokio::time::timeout(
            PROBE_TIMEOUT,
            chain::rpc_call(url, "eth_getTransactionByHash", serde_json::json!([hash])),
        )
        .await
        {
            Ok(Ok(v)) => v.get("result").map(|r| !r.is_null()).unwrap_or(false),
            _ => false,
        };
        if known {
            return true;
        }
    }
    false
}

async fn poll_receipt(tx_id: i64, hash: &str, chain_id: i64) -> AppResult<()> {
    let chains = chain::list_chains()?;
    let chain_row = match chains.into_iter().find(|c| c.chain_id == chain_id) {
        Some(c) => c,
        None => {
            set_tx_status(tx_id, "failed", Some(hash), Some("chain missing"), None);
            return Ok(());
        }
    };
    set_tx_status(tx_id, "confirming", Some(hash), None, None);
    // Rotate across every endpoint the chain row lists: one dead provider must
    // not blind the poll (this used to be the single default URL only).
    let endpoints = chain::endpoint_list(&chain_row.rpc_url);
    let mut attempts: i64 = crate::db::with_conn(|conn| {
        let n: i64 = conn.query_row(
            "SELECT poll_attempts FROM fund_txs WHERE id=?1",
            [tx_id],
            |r| r.get(0),
        )?;
        Ok(n)
    })
    .unwrap_or(0);
    // Rows resumed mid-budget (crash recovery) skip the fast phantom check —
    // their true age is unknown; the over-budget adjudication still applies.
    let mut phantom_checked = attempts >= 5;
    let mut cycle = 0usize;
    let mut consecutive_errs = 0u32;
    loop {
        if attempts >= FUND_POLL_MAX_ATTEMPTS {
            // Over budget. Adjudicate before ANY failure. First a receipt
            // sweep — a tx mined between cycles must be confirmed here, not
            // mis-described as "still in mempool". Then the mempool probe: a
            // tx still known to some node must NOT be marked failed — the
            // natural next step ("run the job again") would send the amount
            // twice. Only a tx absent from every endpoint is provably dead.
            let mut verdict: Option<bool> = None;
            for url in &endpoints {
                if let Ok(Some(receipt)) = chain::get_receipt(url, hash).await {
                    let status = receipt
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("0x1");
                    verdict = Some(status == "0x1");
                    break;
                }
            }
            if let Some(mined) = verdict {
                set_tx_status(
                    tx_id,
                    if mined { "confirmed" } else { "failed" },
                    Some(hash),
                    if mined {
                        None
                    } else {
                        Some("reverted on-chain (found by the final budget sweep)")
                    },
                    None,
                );
                wallet_store::log_activity(
                    "fund.settled",
                    &format!("Fund tx #{tx_id} settled at poll budget — found by final sweep"),
                    None,
                    mined,
                );
                return Ok(());
            }
            if tx_known_anywhere(hash, chain_id, &endpoints).await {
                let _ = crate::db::with_conn(|conn| {
                    conn.execute(
                        "UPDATE fund_txs SET poll_attempts=0, updated_at=?1 WHERE id=?2",
                        rusqlite::params![crate::db::now_ms(), tx_id],
                    )?;
                    Ok(())
                });
                crate::logging::warn(
                    "fund",
                    &format!("Fund tx #{tx_id}: over budget but {hash} is still in a mempool — watching"),
                );
                set_tx_status(
                    tx_id,
                    "confirming",
                    Some(hash),
                    Some("still in mempool after ~5m — keeping watch; re-running the job now would double-send"),
                    None,
                );
                return Ok(());
            }
            crate::logging::warn(
                "fund",
                &format!("Fund tx #{tx_id}: gave up polling {hash} after {attempts} attempts — absent everywhere"),
            );
            set_tx_status(
                tx_id,
                "failed",
                Some(hash),
                Some("no receipt after ~5m — tx is absent from every endpoint's mempool; verify the hash before re-running"),
                None,
            );
            return Ok(());
        }
        attempts += 1;
        let n = attempts;
        let _ = crate::db::with_conn(|conn| {
            conn.execute(
                "UPDATE fund_txs SET poll_attempts=?1, updated_at=?2 WHERE id=?3",
                rusqlite::params![n, crate::db::now_ms(), tx_id],
            )?;
            Ok(())
        });
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        // Fast phantom check (~10s in): the hash was stamped pre-send, so a
        // tx unknown to every node was never accepted (crash before send, or
        // every endpoint rejected). Fail now and free the nonce instead of
        // burning the whole 5-minute budget.
        if !phantom_checked && attempts >= 5 {
            phantom_checked = true;
            if !tx_known_anywhere(hash, chain_id, &endpoints).await {
                release_fund_tx_nonce(tx_id);
                crate::logging::warn(
                    "fund",
                    &format!("Fund tx #{tx_id}: phantom {hash} — never accepted"),
                );
                set_tx_status(
                    tx_id,
                    "failed",
                    Some(hash),
                    Some("tx unknown to every endpoint ~10s after broadcast — the send was never accepted; safe to re-run the job"),
                    None,
                );
                return Ok(());
            }
        }

        let mut answered = false;
        for k in 0..endpoints.len().max(1) {
            let url = &endpoints[(cycle + k) % endpoints.len().max(1)];
            match chain::get_receipt(url, hash).await {
                Ok(Some(receipt)) => {
                    let status = receipt
                        .get("status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("0x1");
                    if status == "0x1" {
                        set_tx_status(tx_id, "confirmed", Some(hash), None, None);
                        crate::logging::info("fund", &format!("Fund tx #{tx_id} confirmed {hash}"));
                        wallet_store::log_activity(
                            "fund.confirmed",
                            &format!("Fund tx #{} confirmed {hash}", tx_id),
                            None,
                            true,
                        );
                    } else {
                        set_tx_status(tx_id, "failed", Some(hash), Some("reverted on-chain"), None);
                        crate::logging::error(
                            "fund",
                            &format!("Fund tx #{tx_id} reverted {hash}"),
                        );
                        wallet_store::log_activity(
                            "fund.failed",
                            &format!("Fund tx #{} reverted {hash}", tx_id),
                            None,
                            false,
                        );
                    }
                    return Ok(());
                }
                Ok(None) => {
                    answered = true;
                    break; // reachable, no receipt yet
                }
                Err(_) => {}
            }
        }
        cycle = cycle.wrapping_add(1);
        if answered {
            consecutive_errs = 0;
        } else {
            consecutive_errs += 1;
            if consecutive_errs >= 15 {
                // Every endpoint failed for ~30s. Leave the row `confirming`
                // (never `failed`) so the runner re-polls — a false `failed`
                // on a live tx is how a manual re-run sends the funds twice.
                crate::logging::warn(
                    "fund",
                    &format!("Fund tx #{tx_id}: receipt RPCs unreachable, will re-poll"),
                );
                set_tx_status(
                    tx_id,
                    "confirming",
                    Some(hash),
                    Some("receipt RPCs unreachable — will re-poll"),
                    None,
                );
                return Ok(());
            }
        }
    }
}

/// Sequential runner body: finish confirming/pending rows in order.
/// Caller must already hold the `FUND_RUNNING` claim (RunGuard).
async fn run_job_loop(job_id: i64) -> AppResult<()> {
    loop {
        let job = match get_job(job_id) {
            Ok(j) if j.status == "running" => j,
            Ok(_) => return Ok(()),
            Err(e) => return Err(e),
        };
        let txs = list_job_txs(job_id)?;
        // Resume broadcasts first, then pending, in seq order.
        let next = txs
            .iter()
            .find(|t| matches!(t.status.as_str(), "broadcasting" | "confirming"))
            .or_else(|| txs.iter().find(|t| t.status == "pending"));
        let Some(tx) = next.cloned() else {
            let _ = crate::db::with_conn(|conn| {
                conn.execute(
                    "UPDATE fund_jobs SET status = 'done', updated_at = ?1 WHERE id = ?2 AND status = 'running'",
                    rusqlite::params![crate::db::now_ms(), job_id],
                )?;
                Ok(())
            });
            refresh_counts(job_id);
            wallet_store::log_activity(
                "fund.done",
                &format!("Fund job #{job_id} finished"),
                None,
                true,
            );
            return Ok(());
        };
        // Throttle: public RPCs rate-limit rapid sequential calls.
        tokio::time::sleep(std::time::Duration::from_millis(1_000)).await;
        if let Err(e) = process_one_tx(&tx, &job).await {
            crate::logging::error("fund", &format!("Fund tx #{} error: {e}", tx.id));
            set_tx_status(tx.id, "failed", None, Some(&e.to_string()), None);
        }
    }
}

async fn token_meta(rpc: &str, token: &str) -> AppResult<(u8, String)> {
    let d = erc20::decimals(rpc, token).await.unwrap_or(18);
    let s = erc20::symbol(rpc, token).await.unwrap_or_else(|_| "TOKEN".into());
    Ok((d, s))
}

/// Public helper for the UI: resolve decimals/symbol for a job asset on resume.
pub async fn asset_meta(chain_id: i64, asset: &str) -> AppResult<(u8, String)> {
    let chains = chain::list_chains()?;
    let row = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;
    let asset = asset.trim();
    if asset == "native" {
        return Ok((18, row.symbol));
    }
    if !is_valid_address(asset) {
        return Err(AppError::Invalid(
            "asset must be 'native' or a 0x token address".into(),
        ));
    }
    let primary = chain::endpoint_list(&row.rpc_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| row.rpc_url.clone());
    token_meta(&primary, asset).await
}

/// Compute per-transfer amounts using live balances (UI preview).
pub async fn preview(a: FundStartArgs) -> AppResult<FundPreview> {
    validate_args(&a)?;
    let chains = chain::list_chains()?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == a.chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {}", a.chain_id)))?;
    let primary = chain::endpoint_list(&chain_row.rpc_url)
        .into_iter()
        .next()
        .unwrap_or_else(|| chain_row.rpc_url.clone());
    let is_native = a.asset.trim() == "native";
    let (decimals, symbol) = if is_native {
        (18u8, chain_row.symbol.clone())
    } else {
        token_meta(&primary, a.asset.trim()).await?
    };
    let configured = parse_u256(&a.amount_wei)?;
    let is_disperse = a.mode == "disperse";
    let anchor = wallet_store::get_wallet(a.anchor_wallet_id)?;

    let gas_price = chain::gas_price(&primary).await.unwrap_or(1_000_000_000);
    let est_gas: u64 = if is_native { 21_000 } else { 65_000 };
    let gas_cost = U256::from(est_gas) * U256::from(gas_price.saturating_mul(110) / 100);

    let mut rows = Vec::new();
    let mut total = U256::ZERO;
    let mut warnings = Vec::new();

    for peer_id in &a.peer_wallet_ids {
        let peer = wallet_store::get_wallet(*peer_id)?;
        let (from, to, sender_addr, dest_addr) = if is_disperse {
            (
                anchor.address.clone(),
                peer.address.clone(),
                anchor.address.clone(),
                peer.address.clone(),
            )
        } else {
            (
                peer.address.clone(),
                anchor.address.clone(),
                peer.address.clone(),
                anchor.address.clone(),
            )
        };

        let sender_native = chain::native_balance(&primary, &sender_addr)
            .await
            .ok()
            .and_then(|h| parse_u256(&h).ok())
            .unwrap_or(U256::ZERO);
        let (sender_token_bal, dest_token_bal) = if is_native {
            let dest_native = chain::native_balance(&primary, &dest_addr)
                .await
                .ok()
                .and_then(|h| parse_u256(&h).ok())
                .unwrap_or(U256::ZERO);
            (sender_native, dest_native)
        } else {
            let sb = erc20::balance_of(&primary, a.asset.trim(), &sender_addr)
                .await
                .unwrap_or(U256::ZERO);
            let dbal = erc20::balance_of(&primary, a.asset.trim(), &dest_addr)
                .await
                .unwrap_or(U256::ZERO);
            (sb, dbal)
        };

        let res = calc_send_wei(
            &a.amount_mode,
            &a.mode,
            configured,
            sender_token_bal,
            dest_token_bal,
            gas_cost,
            is_native,
        );
        match res {
            Ok(send) => {
                total += send;
                rows.push(FundPreviewRow {
                    wallet_id: *peer_id,
                    from_address: from,
                    to_address: to,
                    balance_wei: sender_token_bal.to_string(),
                    send_wei: send.to_string(),
                    skip_reason: None,
                });
            }
            Err(reason) => {
                rows.push(FundPreviewRow {
                    wallet_id: *peer_id,
                    from_address: from,
                    to_address: to,
                    balance_wei: sender_token_bal.to_string(),
                    send_wei: "0".into(),
                    skip_reason: Some(reason.to_string()),
                });
            }
        }

        // Each source must hold enough native for gas (ERC-20 consolidate
        // sources, or any native fixed send that only barely covers amount).
        if sender_native < gas_cost {
            warnings.push(format!(
                "source {} may lack native balance for gas",
                short_addr(&sender_addr)
            ));
        }
    }

    if rows.iter().all(|r| r.skip_reason.is_some()) {
        warnings.push("every transfer would be skipped".into());
    }
    if is_disperse && is_native {
        let need = total + gas_cost * U256::from(a.peer_wallet_ids.len() as u64);
        let anchor_bal = chain::native_balance(&primary, &anchor.address)
            .await
            .ok()
            .and_then(|h| parse_u256(&h).ok())
            .unwrap_or(U256::ZERO);
        if anchor_bal < need {
            warnings.push("source balance may not cover total + gas".into());
        }
    }
    warnings.dedup();

    Ok(FundPreview {
        rows,
        total_send_wei: total.to_string(),
        is_native,
        decimals,
        symbol,
        warnings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Shared with `mint::tests`/e2e — one lock for the process-global DB/vault.
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::db::test_support::serial_guard()
    }

    fn fresh_db(tag: &str) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_fund_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = crate::db::init(&dir.join("f.db"));
        let _ = crate::db::with_conn(|conn| {
            conn.execute("DELETE FROM fund_txs", [])?;
            conn.execute("DELETE FROM fund_jobs", [])?;
            Ok(())
        });
    }

    fn ensure_unlocked() {
        if crate::vault::is_unlocked().unwrap_or(false) {
            return;
        }
        for pass in ["test-pass-123", "correct-horse-1", "aegis-e2e-pass"] {
            if crate::vault::unlock_or_create(pass).is_ok() {
                return;
            }
        }
        panic!("could not unlock vault for fund tests");
    }

    fn ensure_wallet(label: &str) -> i64 {
        if let Ok(list) = crate::wallet_store::list_wallets() {
            if let Some(w) = list.into_iter().find(|w| w.label == label) {
                return w.id;
            }
        }
        match crate::wallet_store::create_wallet(label, None) {
            Ok(w) => w.id,
            Err(_) => crate::wallet_store::list_wallets()
                .unwrap()
                .into_iter()
                .next()
                .expect("wallet")
                .id,
        }
    }

    #[test]
    fn calc_fixed_requires_balance() {
        let amt = U256::from(1_000_000u64);
        assert_eq!(
            calc_send_wei("fixed", "disperse", amt, amt, U256::ZERO, U256::ZERO, true),
            Ok(amt)
        );
        assert!(calc_send_wei(
            "fixed",
            "disperse",
            amt,
            U256::ZERO,
            U256::ZERO,
            U256::ZERO,
            true
        )
        .is_err());
        assert!(calc_send_wei(
            "fixed",
            "disperse",
            U256::ZERO,
            amt,
            U256::ZERO,
            U256::ZERO,
            true
        )
        .is_err());
    }

    #[test]
    fn calc_target_disperse_fills_gap() {
        let target = U256::from(100u64);
        // dest has 40 → send 60
        assert_eq!(
            calc_send_wei("target", "disperse", target, U256::from(1000u64), U256::from(40u64), U256::ZERO, true),
            Ok(U256::from(60u64))
        );
        // already at/above target
        assert!(calc_send_wei(
            "target",
            "disperse",
            target,
            U256::from(1000u64),
            target,
            U256::ZERO,
            true
        )
        .is_err());
        assert!(calc_send_wei(
            "target",
            "disperse",
            target,
            U256::from(1000u64),
            U256::from(200u64),
            U256::ZERO,
            true
        )
        .is_err());
        // sender too poor
        assert!(calc_send_wei(
            "target",
            "disperse",
            target,
            U256::from(10u64),
            U256::ZERO,
            U256::ZERO,
            true
        )
        .is_err());
    }

    #[test]
    fn calc_target_consolidate_native_subtracts_gas() {
        let target = U256::from(100u64);
        let bal = U256::from(1000u64);
        let gas = U256::from(300u64);
        // native: leave target + gas behind
        assert_eq!(
            calc_send_wei("target", "consolidate", target, bal, U256::ZERO, gas, true),
            Ok(U256::from(600u64))
        );
        // erc20: gas is paid in native, token fully swept above target
        assert_eq!(
            calc_send_wei("target", "consolidate", target, bal, U256::ZERO, gas, false),
            Ok(U256::from(900u64))
        );
        // at target → skip
        assert!(calc_send_wei(
            "target",
            "consolidate",
            target,
            target,
            U256::ZERO,
            gas,
            true
        )
        .is_err());
        // dust after gas
        assert!(calc_send_wei(
            "target",
            "consolidate",
            target,
            U256::from(150u64),
            U256::ZERO,
            gas,
            true
        )
        .is_err());
    }

    #[test]
    fn validation_rejects_bad_args() {
        let _serial = serial_guard();
        fresh_db("val");
        ensure_unlocked();
        let a = ensure_wallet("fund-val-a");
        let b = ensure_wallet("fund-val-b");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;

        let ok = FundStartArgs {
            mode: "disperse".into(),
            amount_mode: "fixed".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "1000".into(),
            anchor_wallet_id: a,
            peer_wallet_ids: vec![b],
        };
        assert!(create_job(ok.clone()).is_ok());

        // empty peers
        assert!(create_job(FundStartArgs {
            peer_wallet_ids: vec![],
            ..ok.clone()
        })
        .is_err());
        // anchor in peers
        assert!(create_job(FundStartArgs {
            peer_wallet_ids: vec![a, b],
            ..ok.clone()
        })
        .is_err());
        // bad mode
        assert!(create_job(FundStartArgs {
            mode: "nope".into(),
            ..ok.clone()
        })
        .is_err());
        // bad asset
        assert!(create_job(FundStartArgs {
            asset: "0x123".into(),
            ..ok.clone()
        })
        .is_err());
        // zero fixed amount
        assert!(create_job(FundStartArgs {
            amount_wei: "0".into(),
            ..ok.clone()
        })
        .is_err());
        // unknown chain
        assert!(create_job(FundStartArgs {
            chain_id: 999_999_999,
            ..ok.clone()
        })
        .is_err());
        // unknown wallet
        assert!(create_job(FundStartArgs {
            peer_wallet_ids: vec![999_999],
            ..ok.clone()
        })
        .is_err());
        // duplicate peer
        assert!(create_job(FundStartArgs {
            peer_wallet_ids: vec![b, b],
            ..ok.clone()
        })
        .is_err());
    }

    #[test]
    fn multi_peer_consolidate_signs_each_peer() {
        let _serial = serial_guard();
        fresh_db("mcons");
        ensure_unlocked();
        let anchor = ensure_wallet("fund-mcons-anchor");
        let p1 = ensure_wallet("fund-mcons-p1");
        let p2 = ensure_wallet("fund-mcons-p2");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;

        let job = create_job(FundStartArgs {
            mode: "consolidate".into(),
            amount_mode: "target".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "1000000000000000000".into(),
            anchor_wallet_id: anchor,
            peer_wallet_ids: vec![p1, p2],
        })
        .unwrap();
        let txs = list_job_txs(job.id).unwrap();
        assert_eq!(txs.len(), 2);
        let anchor_addr = crate::wallet_store::get_wallet(anchor).unwrap().address;
        let p1_addr = crate::wallet_store::get_wallet(p1).unwrap().address;
        let p2_addr = crate::wallet_store::get_wallet(p2).unwrap().address;
        // each peer → anchor, signed by that peer (not the anchor)
        assert_eq!(txs[0].from_address, p1_addr);
        assert_eq!(txs[0].to_address, anchor_addr);
        assert_eq!(txs[0].wallet_id, p1);
        assert_eq!(txs[1].from_address, p2_addr);
        assert_eq!(txs[1].to_address, anchor_addr);
        assert_eq!(txs[1].wallet_id, p2);
        assert!(txs.iter().all(|t| t.kind == "consolidate"));
        // target mode leaves amount 0 until runner calc
        assert!(txs.iter().all(|t| t.amount_wei == "0"));
        let _ = stop_job(job.id);
    }

    #[test]
    fn fixed_disperse_persists_amount_before_broadcast() {
        let _serial = serial_guard();
        fresh_db("fxam");
        ensure_unlocked();
        let a = ensure_wallet("fund-fxam-a");
        let b = ensure_wallet("fund-fxam-b");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;
        let job = create_job(FundStartArgs {
            mode: "disperse".into(),
            amount_mode: "fixed".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "555000".into(),
            anchor_wallet_id: a,
            peer_wallet_ids: vec![b],
        })
        .unwrap();
        let txs = list_job_txs(job.id).unwrap();
        assert_eq!(txs.len(), 1);
        assert_eq!(txs[0].amount_wei, "555000");
        let _ = stop_job(job.id);
    }

    #[test]
    fn fixed_native_requires_amount_plus_gas() {
        // pure math: fixed native must fail when balance covers amount but not gas
        assert_eq!(
            calc_send_wei("fixed", "disperse", U256::from(100u64), U256::from(100u64), U256::ZERO, U256::from(1u64), true),
            Err("insufficient for amount + gas")
        );
        assert_eq!(
            calc_send_wei("fixed", "disperse", U256::from(100u64), U256::from(101u64), U256::ZERO, U256::from(1u64), true),
            Ok(U256::from(100u64))
        );
        // ERC-20 fixed ignores native gas in the token balance check
        assert_eq!(
            calc_send_wei("fixed", "disperse", U256::from(100u64), U256::from(100u64), U256::ZERO, U256::from(999u64), false),
            Ok(U256::from(100u64))
        );
    }

    #[test]
    fn create_job_writes_rows_and_stop_skips_pending() {
        let _serial = serial_guard();
        fresh_db("life");
        ensure_unlocked();
        let a = ensure_wallet("fund-life-a");
        let b = ensure_wallet("fund-life-b");
        let c = ensure_wallet("fund-life-c");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;

        let job = create_job(FundStartArgs {
            mode: "disperse".into(),
            amount_mode: "fixed".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "12345".into(),
            anchor_wallet_id: a,
            peer_wallet_ids: vec![b, c],
        })
        .unwrap();
        assert_eq!(job.status, "running");
        assert_eq!(job.total_count, 2);
        assert_eq!(job.peer_wallet_ids, vec![b, c]);

        let txs = list_job_txs(job.id).unwrap();
        assert_eq!(txs.len(), 2);
        assert!(txs.iter().all(|t| t.status == "pending"));
        assert!(txs.iter().all(|t| t.kind == "disperse"));
        // disperse: from anchor → to peer, signed by anchor
        let anchor_addr = crate::wallet_store::get_wallet(a).unwrap().address;
        assert!(txs.iter().all(|t| t.from_address == anchor_addr));
        assert!(
            txs.iter().all(|t| t.wallet_id == a),
            "disperse must be signed by anchor wallet, not peer"
        );

        // active_job sees it
        let active = active_job().unwrap();
        assert!(active.is_some());
        assert_eq!(active.unwrap().id, job.id);

        // stop marks pending skipped
        let stopped = stop_job(job.id).unwrap();
        assert_eq!(stopped.status, "stopped");
        let txs = list_job_txs(job.id).unwrap();
        assert!(txs.iter().all(|t| t.status == "skipped"));
        assert!(active_job().unwrap().is_none());
        // cannot stop again
        assert!(stop_job(job.id).is_err());
    }

    #[test]
    fn consolidate_direction_flips_from_to() {
        let _serial = serial_guard();
        fresh_db("cons");
        ensure_unlocked();
        let anchor = ensure_wallet("fund-cons-anchor");
        let peer = ensure_wallet("fund-cons-peer");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;

        let job = create_job(FundStartArgs {
            mode: "consolidate".into(),
            amount_mode: "target".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "1000000000000000000".into(),
            anchor_wallet_id: anchor,
            peer_wallet_ids: vec![peer],
        })
        .unwrap();
        let txs = list_job_txs(job.id).unwrap();
        assert_eq!(txs.len(), 1);
        let anchor_addr = crate::wallet_store::get_wallet(anchor).unwrap().address;
        let peer_addr = crate::wallet_store::get_wallet(peer).unwrap().address;
        assert_eq!(txs[0].from_address, peer_addr);
        assert_eq!(txs[0].to_address, anchor_addr);
        assert_eq!(txs[0].kind, "consolidate");
        assert_eq!(txs[0].wallet_id, peer, "peer signs the consolidate send");
    }

    #[test]
    fn recover_marks_signing_failed() {
        let _serial = serial_guard();
        fresh_db("rec");
        ensure_unlocked();
        let a = ensure_wallet("fund-rec-a");
        let b = ensure_wallet("fund-rec-b");
        let base = crate::chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 8453)
            .unwrap()
            .chain_id;
        let job = create_job(FundStartArgs {
            mode: "disperse".into(),
            amount_mode: "fixed".into(),
            asset: "native".into(),
            chain_id: base,
            amount_wei: "1".into(),
            anchor_wallet_id: a,
            peer_wallet_ids: vec![b],
        })
        .unwrap();
        let tx = list_job_txs(job.id).unwrap().remove(0);
        // simulate crash mid-sign
        set_tx_status(tx.id, "signing", None, None, None);
        recover_stale();
        let after = list_job_txs(job.id).unwrap().remove(0);
        assert_eq!(after.status, "failed");
        assert!(after.error.as_deref().unwrap().contains("interrupted"));
        // runner respawn may flip job — force-stop leftover running jobs for hygiene
        let _ = crate::db::with_conn(|conn| {
            conn.execute("UPDATE fund_jobs SET status = 'done'", [])?;
            conn.execute("UPDATE fund_txs SET status = 'skipped'", [])?;
            Ok(())
        });
    }
}
