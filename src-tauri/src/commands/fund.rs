use crate::error::AppResult;
use crate::fund::{self, FundJobRow, FundPreview, FundStartArgs, FundTxRow};
use serde::Deserialize;

#[derive(Deserialize)]
pub struct FundsStartArgs {
    pub mode: String,
    pub amount_mode: String,
    pub asset: String,
    pub chain_id: i64,
    pub amount_wei: String,
    pub anchor_wallet_id: i64,
    pub peer_wallet_ids: Vec<i64>,
}

impl From<FundsStartArgs> for FundStartArgs {
    fn from(a: FundsStartArgs) -> Self {
        FundStartArgs {
            mode: a.mode,
            amount_mode: a.amount_mode,
            asset: a.asset,
            chain_id: a.chain_id,
            amount_wei: a.amount_wei,
            anchor_wallet_id: a.anchor_wallet_id,
            peer_wallet_ids: a.peer_wallet_ids,
        }
    }
}

#[tauri::command]
pub async fn funds_preview(args: FundsStartArgs) -> AppResult<FundPreview> {
    fund::preview(args.into()).await
}

#[tauri::command]
pub async fn funds_start(args: FundsStartArgs) -> AppResult<FundJobRow> {
    fund::start(args.into())
}

#[tauri::command]
pub fn funds_job(id: i64) -> AppResult<FundJobRow> {
    fund::get_job(id)
}

#[tauri::command]
pub fn funds_job_txs(job_id: i64) -> AppResult<Vec<FundTxRow>> {
    fund::list_job_txs(job_id)
}

#[tauri::command]
pub fn funds_active() -> AppResult<Option<FundJobRow>> {
    fund::active_job()
}

#[tauri::command]
pub fn funds_stop(job_id: i64) -> AppResult<FundJobRow> {
    fund::stop_job(job_id)
}

#[tauri::command]
pub async fn funds_asset_meta(chain_id: i64, asset: String) -> AppResult<(u8, String)> {
    fund::asset_meta(chain_id, &asset).await
}
