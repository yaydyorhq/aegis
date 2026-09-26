use crate::error::AppResult;
use crate::vault;
use crate::wallet_store::{self, WalletRow};
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
pub fn wallet_import(label: String, private_key: String) -> AppResult<WalletRow> {
    wallet_store::create_wallet(&label, Some(&private_key))
}

#[tauri::command]
pub fn wallet_delete(id: i64) -> AppResult<()> {
    wallet_store::delete_wallet(id)
}

#[tauri::command]
pub fn wallet_export(id: i64, pass_confirm: String) -> AppResult<String> {
    wallet_store::export_private_key(id, &pass_confirm)
}
