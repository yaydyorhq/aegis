use crate::error::{AppError, AppResult};
use crate::mint::allowlist::{self, AllowlistEntryInfo};
use crate::mint::{self, seadrop, EnqueueArgs, MintTaskRow};
use crate::mint::seadrop::SeaDropPlan;
use crate::opensea::{self, OpenSeaClient, OpenSeaMintPlan};
use crate::wallet_store;
use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

#[tauri::command]
pub fn mint_list() -> AppResult<Vec<MintTaskRow>> {
    mint::list_tasks()
}

/// Read SeaDrop public drop for an NFT contract and build mintPublic calldata.
/// The plan's `to` is always the SeaDrop singleton — not the NFT proxy.
#[tauri::command]
pub async fn mint_seadrop_plan(
    chain_id: i64,
    nft_contract: String,
    quantity: i64,
) -> AppResult<SeaDropPlan> {
    let chain = crate::chain::list_chains()?
        .into_iter()
        .find(|c| c.id == chain_id || c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;
    let plan = seadrop::build_public_mint_plan(&chain.rpc_url, &nft_contract, quantity).await?;
    wallet_store::log_activity(
        "mint.seadrop_plan",
        &format!(
            "SeaDrop plan nft={} qty={} value={} ETH live={}",
            plan.nft_contract, plan.quantity, plan.value_eth, plan.live
        ),
        None,
        true,
    );
    Ok(plan)
}

/// Parse allowlist text/JSON and return entry summaries (address + flags).
#[tauri::command]
pub fn mint_allowlist_parse(allowlist: String) -> AppResult<Vec<AllowlistEntryInfo>> {
    let list = allowlist::parse_allowlist(&allowlist)?;
    Ok(list.iter().map(|e| e.info()).collect())
}

#[derive(Serialize)]
pub struct AllowlistMatchRow {
    pub wallet_id: i64,
    pub address: String,
    pub matched: bool,
    pub has_proof: bool,
    pub has_signature: bool,
    pub has_calldata: bool,
}

/// Match selected wallets against a pasted allowlist (GTD preview).
#[tauri::command]
pub fn mint_allowlist_match(
    wallet_ids: Vec<i64>,
    allowlist: String,
) -> AppResult<Vec<AllowlistMatchRow>> {
    let list = allowlist::parse_allowlist(&allowlist)?;
    let mut out = Vec::with_capacity(wallet_ids.len());
    for id in wallet_ids {
        let w = wallet_store::get_wallet(id)?;
        let entry = allowlist::find_entry(&list, &w.address);
        out.push(AllowlistMatchRow {
            wallet_id: w.id,
            address: w.address,
            matched: entry.is_some(),
            has_proof: entry.map(|e| e.info().has_proof).unwrap_or(false),
            has_signature: entry.map(|e| e.info().has_signature).unwrap_or(false),
            has_calldata: entry.map(|e| e.info().has_calldata).unwrap_or(false),
        });
    }
    Ok(out)
}

#[derive(Deserialize)]
pub struct MintEnqueueArgs {
    pub wallet_ids: Vec<i64>,
    pub chain_id: i64,
    pub contract: String,
    pub quantity: i64,
    pub value_wei: Option<String>,
    pub preset: Option<String>,
    pub function_name: Option<String>,
    pub is_hex: Option<bool>,
    pub parameters: Option<String>,
    pub calldata: Option<String>,
    pub rpc_endpoints: Option<Vec<String>>,
    pub flashbots: Option<bool>,
    pub gas_limit: Option<String>,
    pub max_fee_gwei: Option<String>,
    pub priority_fee_gwei: Option<String>,
    pub nonce_override: Option<String>,
    pub scheduled_at: Option<i64>,
    pub delay_ms: Option<i64>,
    pub mode: Option<String>,
    /// Optional allowlist JSON/text — filters wallets and injects proof/signature/calldata.
    pub allowlist: Option<String>,
}

#[derive(Serialize)]
pub struct SkippedWallet {
    pub wallet_id: i64,
    pub address: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct EnqueueBatchResult {
    pub tasks: Vec<MintTaskRow>,
    pub skipped: Vec<SkippedWallet>,
}

fn apply_entry_to_enqueue(
    mut a: EnqueueArgs,
    wallet_address: &str,
    entry: &allowlist::AllowlistEntry,
) -> AppResult<EnqueueArgs> {
    if let Some(cd) = &entry.calldata {
        a.is_hex = true;
        a.calldata = Some(cd.clone());
        a.function_name = None;
        a.parameters = None;
        return Ok(a);
    }
    if a.is_hex {
        let template = a.calldata.clone().ok_or_else(|| {
            AppError::Invalid("hex calldata required when HEX is checked".into())
        })?;
        a.calldata = Some(allowlist::resolve_hex_template(
            &template,
            wallet_address,
            Some(entry),
        )?);
        a.function_name = None;
        a.parameters = None;
        return Ok(a);
    }
    let sig = a
        .function_name
        .clone()
        .unwrap_or_else(|| "mint()".into());
    let cd = mint::build_calldata_with_entry(
        &sig,
        a.parameters.as_deref(),
        a.quantity,
        wallet_address,
        Some(entry),
    )?;
    a.is_hex = true;
    a.calldata = Some(cd);
    a.function_name = None;
    a.parameters = None;
    Ok(a)
}

#[tauri::command]
pub fn mint_enqueue(args: MintEnqueueArgs) -> AppResult<EnqueueBatchResult> {
    if args.wallet_ids.is_empty() {
        return Err(AppError::Invalid("select at least one wallet".into()));
    }
    let al = args
        .allowlist
        .as_deref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(allowlist::parse_allowlist)
        .transpose()?;

    let function_name = args
        .function_name
        .clone()
        .or_else(|| args.preset.clone())
        .or_else(|| Some("mint()".into()));
    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for wallet_id in args.wallet_ids.clone() {
        let wallet = wallet_store::get_wallet(wallet_id)?;
        let entry = match &al {
            None => None,
            Some(list) => match allowlist::find_entry(list, &wallet.address) {
                Some(e) => Some(e.clone()),
                None => {
                    skipped.push(SkippedWallet {
                        wallet_id,
                        address: wallet.address,
                        reason: "not on allowlist".into(),
                    });
                    continue;
                }
            },
        };

        let mut a = EnqueueArgs {
            wallet_id,
            chain_id: args.chain_id,
            contract: args.contract.clone(),
            quantity: args.quantity,
            value_wei: args.value_wei.clone(),
            function_name: function_name.clone(),
            is_hex: args.is_hex.unwrap_or(false),
            parameters: args.parameters.clone(),
            calldata: args.calldata.clone(),
            rpc_endpoints: args.rpc_endpoints.clone(),
            flashbots: args.flashbots.unwrap_or(false),
            gas_limit: args.gas_limit.clone(),
            max_fee_gwei: args.max_fee_gwei.clone(),
            priority_fee_gwei: args.priority_fee_gwei.clone(),
            nonce_override: args.nonce_override.clone(),
            scheduled_at: args.scheduled_at,
            delay_ms: args.delay_ms,
            mode: args.mode.clone(),
        };
        if let Some(entry) = &entry {
            a = apply_entry_to_enqueue(a, &wallet.address, entry)?;
        } else if a.is_hex {
            if let Some(cd) = a.calldata.as_deref() {
                if cd.contains("{signature}") || cd.contains("{proof}") {
                    // Skip this wallet instead of aborting the whole batch
                    // mid-enqueue (partial queues were confusing to recover).
                    skipped.push(SkippedWallet {
                        wallet_id,
                        address: wallet.address,
                        reason: "{signature}/{proof} require an allowlist with that entry"
                            .into(),
                    });
                    continue;
                }
            }
        }
        out.push(mint::enqueue(a)?);
    }
    if al.is_some() {
        wallet_store::log_activity(
            "mint.allowlist",
            &format!(
                "Allowlist filter: {} queued, {} skipped",
                out.len(),
                skipped.len()
            ),
            None,
            true,
        );
    }
    Ok(EnqueueBatchResult {
        tasks: out,
        skipped,
    })
}

/// Preview one wallet's OpenSea stage mint (SIWE + stages + MintAction calldata).
#[tauri::command]
pub async fn mint_opensea_plan(
    chain_id: i64,
    wallet_id: i64,
    collection: String,
    quantity: i64,
    token_id: Option<String>,
) -> AppResult<OpenSeaMintPlan> {
    if quantity < 1 {
        return Err(AppError::Invalid("quantity must be >= 1".into()));
    }
    let chain = crate::chain::list_chains()?
        .into_iter()
        .find(|c| c.id == chain_id || c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;
    let expected = u64::try_from(chain.chain_id).unwrap_or(0);
    let wallet = wallet_store::get_wallet(wallet_id)?;
    let qty = u64::try_from(quantity).unwrap_or(1);
    let plan = opensea::plan_mint(
        wallet_id,
        &wallet.address,
        &collection,
        expected,
        qty,
        token_id.as_deref(),
    )
    .await?;
    wallet_store::log_activity(
        "mint.opensea_plan",
        &format!(
            "OpenSea plan stage={} qty={} value={} ETH slug={}",
            plan.stage_type, plan.quantity, plan.value_eth, plan.slug
        ),
        None,
        true,
    );
    Ok(plan)
}

#[derive(Deserialize)]
pub struct MintOpenSeaEnqueueArgs {
    pub wallet_ids: Vec<i64>,
    pub chain_id: i64,
    /// NFT collection contract address (resolved via OpenSea).
    pub collection: String,
    pub quantity: i64,
    pub token_id: Option<String>,
    pub rpc_endpoints: Option<Vec<String>>,
    pub flashbots: Option<bool>,
    pub gas_limit: Option<String>,
    pub max_fee_gwei: Option<String>,
    pub priority_fee_gwei: Option<String>,
    pub scheduled_at: Option<i64>,
    pub delay_ms: Option<i64>,
    pub mode: Option<String>,
}

/// Per-wallet OpenSea stage mint: SIWE → eligible stage → MintAction → mint queue.
#[tauri::command]
pub async fn mint_opensea_enqueue(args: MintOpenSeaEnqueueArgs) -> AppResult<EnqueueBatchResult> {
    if args.wallet_ids.is_empty() {
        return Err(AppError::Invalid("select at least one wallet".into()));
    }
    if !crate::vault::is_unlocked().unwrap_or(false) {
        return Err(AppError::VaultLocked);
    }
    if args.quantity < 1 {
        return Err(AppError::Invalid("quantity must be >= 1".into()));
    }
    let chain = crate::chain::list_chains()?
        .into_iter()
        .find(|c| c.id == args.chain_id || c.chain_id == args.chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {}", args.chain_id)))?;
    let expected = u64::try_from(chain.chain_id).unwrap_or(0);
    let qty = u64::try_from(args.quantity).unwrap_or(1);
    let token_id = args.token_id.clone().unwrap_or_else(|| "0".into());

    let client = OpenSeaClient::new()?;
    let resolved = client
        .resolve_collection(&args.collection, Some(expected))
        .await?
        .ok_or_else(|| AppError::NotFound("collection not on OpenSea".into()))?;

    let mut out = Vec::new();
    let mut skipped = Vec::new();
    for wallet_id in args.wallet_ids.clone() {
        let wallet = match wallet_store::get_wallet(wallet_id) {
            Ok(w) => w,
            Err(e) => {
                skipped.push(SkippedWallet {
                    wallet_id,
                    address: String::new(),
                    reason: format!("wallet not found: {e}"),
                });
                continue;
            }
        };
        let addr: Address = match wallet.address.parse() {
            Ok(a) => a,
            Err(_) => {
                skipped.push(SkippedWallet {
                    wallet_id,
                    address: wallet.address,
                    reason: "bad wallet address".into(),
                });
                continue;
            }
        };
        let plan = match client
            .plan_wallet_mint(wallet_id, &addr, &resolved, expected, qty, &token_id)
            .await
        {
            Ok(p) => p,
            Err(e) => {
                skipped.push(SkippedWallet {
                    wallet_id,
                    address: wallet.address,
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let enqueue = EnqueueArgs {
            wallet_id,
            chain_id: args.chain_id,
            contract: plan.to.clone(),
            quantity: plan.quantity,
            value_wei: Some(plan.value_wei.clone()),
            function_name: None,
            is_hex: true,
            parameters: None,
            calldata: Some(plan.calldata.clone()),
            rpc_endpoints: args.rpc_endpoints.clone(),
            flashbots: args.flashbots.unwrap_or(false),
            gas_limit: args.gas_limit.clone(),
            max_fee_gwei: args.max_fee_gwei.clone(),
            priority_fee_gwei: args.priority_fee_gwei.clone(),
            nonce_override: None,
            scheduled_at: args.scheduled_at,
            delay_ms: args.delay_ms,
            mode: args.mode.clone(),
        };
        match mint::enqueue(enqueue) {
            Ok(task) => out.push(task),
            Err(e) => skipped.push(SkippedWallet {
                wallet_id,
                address: wallet.address,
                reason: format!("enqueue failed: {e}"),
            }),
        }
    }
    wallet_store::log_activity(
        "mint.opensea",
        &format!(
            "OpenSea stage mint: {} queued, {} skipped ({}, stage via MintAction)",
            out.len(),
            skipped.len(),
            resolved.slug
        ),
        None,
        !out.is_empty(),
    );
    Ok(EnqueueBatchResult {
        tasks: out,
        skipped,
    })
}

#[tauri::command]
pub fn mint_cancel(id: i64) -> AppResult<()> {
    mint::cancel_task(id)
}

#[tauri::command]
pub async fn mint_run() -> AppResult<Vec<MintTaskRow>> {
    mint::run_pending().await
}

/// Encode calldata snapshot for one wallet + function signature.
/// Used by the UI "Encode" button so users can preview the exact hex
/// (and fix template mistakes) before enqueueing a batch.
#[tauri::command]
pub fn mint_encode_calldata(
    function_name: String,
    parameters: String,
    quantity: i64,
    wallet_address: String,
) -> AppResult<String> {
    mint::build_calldata_from_fn(
        &function_name,
        Some(&parameters),
        quantity,
        &wallet_address,
    )
}

