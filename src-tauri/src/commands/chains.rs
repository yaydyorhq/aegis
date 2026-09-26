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
