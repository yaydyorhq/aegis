use crate::chain::{self, ChainInput, ChainRow, RpcTestResult};
use crate::error::AppResult;

#[tauri::command]
pub fn chain_list() -> AppResult<Vec<ChainRow>> {
    chain::list_chains()
}

/// Accepts camelCase from JS (chainId, rpcUrl, …) as well as snake_case.
#[tauri::command]
pub fn chain_upsert(chain: ChainInput) -> AppResult<ChainRow> {
    chain::upsert_chain(&chain)
}

#[tauri::command]
pub fn chain_delete(id: i64) -> AppResult<()> {
    chain::delete_chain(id)
}

#[tauri::command]
pub async fn chain_test(id: i64) -> AppResult<RpcTestResult> {
    chain::test_chain(id).await
}

#[derive(serde::Serialize)]
pub struct RpcHealthProbeResult {
    pub url: String,
    pub chain_id: Option<i64>,
    pub block_number: Option<u64>,
    pub eth_call_ok: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// Probe a single RPC endpoint for chainId + eth_call capability.
#[tauri::command]
pub async fn chain_probe(rpc_url: String) -> AppResult<RpcHealthProbeResult> {
    let (cid, block, call_ok, latency, err) = crate::chain::rpc_health_probe(&rpc_url).await;
    Ok(RpcHealthProbeResult {
        url: rpc_url,
        chain_id: cid,
        block_number: block,
        eth_call_ok: call_ok,
        latency_ms: latency,
        error: err,
    })
}
