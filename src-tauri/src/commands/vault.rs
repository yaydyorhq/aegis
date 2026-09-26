use crate::error::AppResult;
use crate::vault;
use crate::wallet_store::{
    self, BulkImportItem, BulkImportResultItem, GroupRow, WalletRow,
};
use serde::Serialize;

#[derive(Serialize)]
pub struct VaultStatus {
    pub initialized: bool,
    pub unlocked: bool,
}

#[tauri::command]
pub fn vault_status() -> AppResult<VaultStatus> {
    Ok(VaultStatus {
        initialized: vault::is_initialized()?,
        unlocked: vault::is_unlocked()?,
    })
}

#[tauri::command]
pub fn vault_unlock(pass: String) -> AppResult<()> {
    vault::unlock_or_create(&pass)?;
    wallet_store::log_activity("vault.unlock", "Vault unlocked", None, true);
    Ok(())
}

#[tauri::command]
pub fn vault_lock() -> AppResult<()> {
    vault::lock()?;
    wallet_store::log_activity("vault.lock", "Vault locked", None, true);
    Ok(())
}

#[tauri::command]
pub fn wallet_list() -> AppResult<Vec<WalletRow>> {
    wallet_store::list_wallets()
}

#[tauri::command]
pub fn wallet_generate(label: String) -> AppResult<WalletRow> {
    wallet_store::create_wallet(&label, None)
}

#[tauri::command]
pub fn wallet_generate_in_group(label: String, group_id: Option<i64>) -> AppResult<WalletRow> {
    wallet_store::create_wallet_in_group(&label, None, group_id)
}

#[tauri::command]
pub fn wallet_import(label: String, private_key: String) -> AppResult<WalletRow> {
    wallet_store::create_wallet(&label, Some(&private_key))
}

#[tauri::command]
pub fn wallet_import_in_group(
    label: String,
    private_key: String,
    group_id: Option<i64>,
) -> AppResult<WalletRow> {
    wallet_store::create_wallet_in_group(&label, Some(&private_key), group_id)
}

#[tauri::command]
pub fn wallet_import_bulk(
    items: Vec<BulkImportItem>,
    group_id: Option<i64>,
) -> AppResult<Vec<BulkImportResultItem>> {
    wallet_store::import_bulk(&items, group_id)
}

#[tauri::command]
pub fn wallet_delete(id: i64) -> AppResult<()> {
    wallet_store::delete_wallet(id)
}

#[tauri::command]
pub fn wallet_export(id: i64, pass_confirm: String) -> AppResult<String> {
    wallet_store::export_private_key(id, &pass_confirm)
}

#[tauri::command]
pub fn wallet_set_group(wallet_id: i64, group_id: Option<i64>) -> AppResult<()> {
    wallet_store::set_wallet_group(wallet_id, group_id)
}

#[tauri::command]
pub fn wallet_rename(id: i64, label: String) -> AppResult<WalletRow> {
    wallet_store::rename_wallet(id, &label)
}

// ── Groups ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn group_list() -> AppResult<Vec<GroupRow>> {
    wallet_store::list_groups()
}

#[tauri::command]
pub fn group_create(name: String) -> AppResult<GroupRow> {
    wallet_store::create_group(&name)
}

#[tauri::command]
pub fn group_rename(id: i64, name: String) -> AppResult<GroupRow> {
    wallet_store::rename_group(id, &name)
}

#[tauri::command]
pub fn group_delete(id: i64) -> AppResult<()> {
    wallet_store::delete_group(id)
}
