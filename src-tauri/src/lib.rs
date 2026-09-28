mod chain;
mod collection_pnl;
mod commands;
mod db;
#[cfg(test)]
mod e2e_integration_tests;
mod error;
mod fund;
mod mint;
mod opensea;
mod pnl;
mod vault;
mod wallet;
mod wallet_store;

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let db_path = dir.join("aegis.db");
            db::init(&db_path)?;
            mint::recover_stale_tasks();
            fund::recover_stale();
            // Auto-run scheduler: fire due scheduled tasks without a manual "Run queue".
            // 60ms keeps worst-case fire lateness under ~60ms; a scheduled phase
            // opener loses FCFS races to coarser ticks (was 250ms). count_runnable
            // is a single indexed COUNT, so the extra wakeups are negligible.
            tauri::async_runtime::spawn(async {
                let mut tick = tokio::time::interval(std::time::Duration::from_millis(60));
                loop {
                    tick.tick().await;
                    // Prep lead (T-10s): warm RPC sockets, nonce/gas and OpenSea
                    // metadata for tasks that are about to fire. Fire and forget —
                    // it must never delay the actual run.
                    match mint::due_for_prep(db::now_ms()) {
                        Ok(ids) => {
                            for id in ids {
                                tauri::async_runtime::spawn(async move {
                                    mint::prep_task(id).await;
                                });
                            }
                        }
                        Err(e) => {
                            wallet_store::log_activity(
                                "mint.prep",
                                &format!("Prep scan error: {e}"),
                                None,
                                false,
                            );
                        }
                    }
                    match mint::count_runnable() {
                        Ok(n) if n > 0 && !mint::is_running() => {
                            if let Err(e) = mint::run_pending().await {
                                if !e.to_string().contains("already in progress") {
                                    wallet_store::log_activity(
                                        "mint.autorun",
                                        &format!("Auto-run error: {e}"),
                                        None,
                                        false,
                                    );
                                }
                            }
                        }
                        Ok(_) => {}
                        Err(e) => {
                            wallet_store::log_activity(
                                "mint.autorun",
                                &format!("Auto-run count error: {e}"),
                                None,
                                false,
                            );
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // vault + wallet
            commands::vault::vault_status,
            commands::vault::vault_unlock,
            commands::vault::vault_lock,
            commands::vault::wallet_list,
            commands::vault::wallet_generate,
            commands::vault::wallet_generate_in_group,
            commands::vault::wallet_import,
            commands::vault::wallet_import_in_group,
            commands::vault::wallet_import_bulk,
            commands::vault::wallet_delete,
            commands::vault::wallet_export,
            commands::vault::wallet_set_group,
            commands::vault::wallet_rename,
            // wallet groups
            commands::vault::group_list,
            commands::vault::group_create,
            commands::vault::group_rename,
            commands::vault::group_delete,
            // chains
            commands::chains::chain_list,
            commands::chains::chain_upsert,
            commands::chains::chain_delete,
            commands::chains::chain_test,
            commands::chains::chain_probe,
            // nft + gallery
            commands::nft::nft_scan,
            commands::nft::gallery_list,
            commands::nft::nft_metadata,
            // eligibility
            commands::eligibility::eligibility_run,
            commands::eligibility::eligibility_matrix_run,
            commands::eligibility::eligibility_history,
            // mint
            commands::mint::mint_list,
            commands::mint::mint_enqueue,
            commands::mint::mint_cancel,
            commands::mint::mint_run,
            commands::mint::mint_promote,
            commands::mint::mint_promote_all,
            commands::mint::mint_retry,
            commands::mint::mint_retry_all,
            commands::mint::mint_seadrop_plan,
            commands::mint::mint_opensea_plan,
            commands::mint::mint_opensea_enqueue,
            commands::mint::mint_allowlist_parse,
            commands::mint::mint_allowlist_match,
            commands::mint::mint_encode_calldata,
            // funds (disperse / consolidate)
            commands::fund::funds_preview,
            commands::fund::funds_start,
            commands::fund::funds_job,
            commands::fund::funds_job_txs,
            commands::fund::funds_active,
            commands::fund::funds_stop,
            commands::fund::funds_asset_meta,
            // activity + stats
            commands::activity::activity_list,
            commands::stats::stats_overview,
            commands::stats::module_status,
            // meta (local prefs)
            commands::meta::meta_get,
            commands::meta::meta_set,
            // api keys
            commands::api_keys::api_key_set,
            commands::api_keys::api_key_list,
            commands::api_keys::api_key_delete,
            // pnl
            commands::pnl::pnl_scan,
            commands::pnl::pnl_last_scan,
            commands::pnl::pnl_history,
            commands::pnl::collection_pnl_scan,
commands::pnl::collection_pnl_last,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
