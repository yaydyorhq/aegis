use crate::chain;
use crate::error::AppResult;
use crate::wallet_store;
use serde::Serialize;
use tauri::Emitter;

fn emit_progress(app: &tauri::AppHandle, phase: &str, done: u64, total: u64) {
    let _ = app.emit(
        "nft-scan-progress",
        serde_json::json!({ "phase": phase, "done": done, "total": total }),
    );
}

#[derive(Serialize)]
pub struct NftScanResult {
    pub wallet_id: Option<i64>,
    pub address: String,
    pub chain_id: i64,
    pub items: Vec<crate::chain::nft::NftItem>,
    pub scanned: usize,
}

#[tauri::command]
pub async fn nft_scan(
    app: tauri::AppHandle,
    wallet_id: Option<i64>,
    address: Option<String>,
    chain_id: i64,
    window_blocks: Option<u64>,
) -> AppResult<NftScanResult> {
    let addr = match (address, wallet_id) {
        (Some(a), _) => a,
        (None, Some(id)) => wallet_store::get_wallet(id)?.address,
        (None, None) => {
            return Err(crate::error::AppError::Invalid(
                "address or wallet_id required".into(),
            ))
        }
    };
    let window = window_blocks.unwrap_or(0);
    emit_progress(&app, "logs", 0, 0);
    let progress_app = app.clone();
    let on_progress = move |done: u64, total: u64| {
        emit_progress(&progress_app, "logs", done, total);
    };
    let owned = crate::chain::nft::scan_wallet_nfts(&addr, chain_id, window, &on_progress).await?;

    let chains = chain::list_chains()?;
    let chain_row = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| crate::error::AppError::NotFound(format!("chain {chain_id}")))?;

    // OpenSea presence per unique contract (best-effort): if the collection isn't
    // listed on OpenSea, items simply get no link.
    emit_progress(&app, "opensea", 0, 0);
    let mut opensea_ident: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    if let Ok(os) = crate::opensea::OpenSeaClient::new() {
        let mut uniq: Vec<String> = owned.iter().map(|(c, _)| c.clone()).collect();
        uniq.sort();
        uniq.dedup();
        for contract in &uniq {
            let ident = match os
                .resolve_collection(contract, Some(chain_id as u64))
                .await
            {
                Ok(Some(r)) if !r.chain_identifier.is_empty() => Some(r.chain_identifier),
                Ok(Some(_)) => {
                    crate::chain::nft::opensea_chain_slug(chain_id).map(str::to_string)
                }
                _ => None,
            };
            opensea_ident.insert(contract.clone(), ident);
        }
    }
    emit_progress(&app, "opensea", 1, 1);

    let total = owned.len() as u64;
    let mut items = Vec::new();
    let mut cache_rows: Vec<crate::chain::nft::NftCacheRow> = Vec::new();
    for (i, (contract, token_id)) in owned.iter().enumerate() {
        emit_progress(&app, "metadata", i as u64, total);
        let uri = crate::chain::nft::token_uri(&chain_row.rpc_url, contract, token_id)
            .await
            .ok()
            .flatten();
        let mut name = None;
        let mut image = None;
        if let Some(u) = uri.as_deref() {
            if let Ok((n, img)) = crate::chain::nft::fetch_metadata(u).await {
                name = n;
                image = img;
            }
        }
        let opensea_url = opensea_ident
            .get(contract)
            .and_then(|o| o.as_ref())
            .map(|ident| crate::chain::nft::opensea_asset_url(ident, contract, token_id));
        cache_rows.push((
            contract.clone(),
            token_id.clone(),
            uri.clone(),
            image.clone(),
            name.clone(),
            opensea_url.clone(),
        ));
        items.push(crate::chain::nft::NftItem {
            id: 0,
            wallet_id,
            chain_id,
            contract: contract.clone(),
            token_id: token_id.clone(),
            uri,
            image,
            name,
            opensea_url,
            fetched_at: crate::db::now_ms(),
        });
    }
    emit_progress(&app, "metadata", total, total);
    crate::chain::nft::cache_nfts(wallet_id, chain_id, &cache_rows)?;
    wallet_store::log_activity(
        "nft.scan",
        &format!(
            "Scanned {} on chain {} — {} NFT(s)",
            addr,
            chain_id,
            items.len()
        ),
        None,
        true,
    );
    Ok(NftScanResult {
        wallet_id,
        address: addr,
        chain_id,
        scanned: items.len(),
        items,
    })
}

#[tauri::command]
pub fn gallery_list(wallet_id: Option<i64>) -> AppResult<Vec<crate::chain::nft::NftItem>> {
    crate::chain::nft::list_cached_nfts(wallet_id)
}

#[tauri::command]
pub async fn nft_metadata(uri: String) -> AppResult<(Option<String>, Option<String>)> {
    crate::chain::nft::fetch_metadata(&uri).await
}
