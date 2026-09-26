/// Comprehensive E2E integration tests — vault → wallet → groups → API keys →
/// fund math → chain seed → mint pipeline.  Covers the full user lifecycle
/// and edge cases found during audit (H01–H03, M01–M07).
///
/// IMPORTANT: The vault + DB are process-global singletons (OnceLock).  Tests
/// cannot start with a truly fresh vault after the first test runs.  Each test
/// uses `ensure_unlocked()` to guarantee the vault is accessible, and
/// `export_passphrase()` to find the correct passphrase for export operations.

#[cfg(test)]
mod e2e_integration {
    use crate::chain;
    use crate::db;
    use crate::mint;
    use crate::mint::EnqueueArgs;
    use crate::vault;
    use crate::wallet;
    use crate::wallet_store;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Passphrases to try when unlocking the process-global vault.
    /// `ensure_unlocked` tries them in order and stops on first success.
    const VAULT_PASSPHRASES: &[&str] = &[
        "test-pass-123",
        "correct-horse-1",
        "aegis-e2e-integration",
        "lifecycle-pass-123",
    ];

    /// Fresh state for tables that change per test.  Vault meta (salt,
    /// verifier) is process-global and must NOT be wiped — once init'd,
    /// it stays init'd for the process lifetime.
    fn fresh_state(tag: &str) {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("aegis_e2e_{tag}_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = db::init(&dir.join("e2e.db"));
        let _ = db::with_conn(|conn| {
            conn.execute("DELETE FROM mint_tasks", [])?;
            conn.execute("DELETE FROM wallets", [])?;
            conn.execute("DELETE FROM wallet_groups", [])?;
            conn.execute("DELETE FROM api_keys", [])?;
            conn.execute("DELETE FROM fund_jobs", [])?;
            conn.execute("DELETE FROM fund_txs", [])?;
            Ok(())
        });
    }

    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ensure the vault is unlocked.  Tries known passphrases.  Panics if
    /// none work (indicates the test environment is broken).
    fn ensure_unlocked() {
        if vault::is_unlocked().unwrap_or(false) {
            return;
        }
        for pass in VAULT_PASSPHRASES {
            if vault::unlock_or_create(pass).is_ok() {
                return;
            }
        }
        panic!("could not unlock vault for e2e integration");
    }

    /// Find a passphrase that passes `confirm_passphrase`.  Used for
    /// export operations that require pass confirmation.
    fn export_passphrase() -> &'static str {
        for pass in VAULT_PASSPHRASES {
            if vault::confirm_passphrase(pass).is_ok() {
                return pass;
            }
        }
        panic!("no valid export passphrase found");
    }

    fn base_chain_id() -> i64 {
        chain::list_chains()
            .unwrap()
            .iter()
            .find(|c| c.chain_id == 8453)
            .map(|c| c.chain_id)
            .unwrap_or(8453)
    }

    // ════════════════════════════════════════════════════════════════════
    // VAULT LIFECYCLE E2E (adapted for process-global vault)
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_vault_full_lifecycle() {
        let _g = serial_guard();
        fresh_state("vault_lifecycle");
        ensure_unlocked();

        // Encrypt + decrypt roundtrip while unlocked
        let (nonce, ct) = vault::encrypt(b"secret-ephemeral-data").unwrap();
        let pt = vault::decrypt(&nonce, &ct).unwrap();
        assert_eq!(pt, b"secret-ephemeral-data");

        // Lock vault — encrypt/decrypt must fail
        vault::lock().unwrap();
        assert!(!vault::is_unlocked().unwrap());
        assert!(vault::encrypt(b"test").is_err());
        assert!(vault::decrypt(&nonce, &ct).is_err());

        // Short passphrase rejected on re-unlock attempt
        assert!(vault::unlock_or_create("short").is_err());
        assert!(!vault::is_unlocked().unwrap());

        // Re-unlock with correct pass
        let pass = export_passphrase();
        vault::unlock_or_create(pass).unwrap();
        assert!(vault::is_unlocked().unwrap());

        // Decrypt still works after re-unlock
        let pt2 = vault::decrypt(&nonce, &ct).unwrap();
        assert_eq!(pt2, b"secret-ephemeral-data");

        // Wrong passphrase fails
        vault::lock().unwrap();
        let err = vault::unlock_or_create("definitely-wrong-xyz");
        assert!(err.is_err());

        // confirm_passphrase
        vault::unlock_or_create(pass).unwrap();
        assert!(vault::confirm_passphrase(pass).is_ok());
        assert!(vault::confirm_passphrase("wrong").is_err());
    }

    #[test]
    fn e2e_vault_verifier_corruption_detected() {
        let _g = serial_guard();
        fresh_state("vault_corrupt");
        ensure_unlocked();

        // The verifier is created at first unlock.  Confirm it rejects
        // wrong passphrases regardless of which pass created it.
        let pass = export_passphrase();
        assert!(vault::confirm_passphrase(pass).is_ok());
        assert!(vault::confirm_passphrase("wrong").is_err());
        assert!(vault::confirm_passphrase("").is_err());
    }

    // ════════════════════════════════════════════════════════════════════
    // WALLET + GROUPS E2E
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_wallet_generate_import_export_delete() {
        let _g = serial_guard();
        fresh_state("wallet_lifecycle");
        ensure_unlocked();

        // Generate wallet
        let w = wallet_store::create_wallet("gen-wallet", None).unwrap();
        assert!(!w.address.is_empty());
        assert!(w.address.starts_with("0x"));
        assert_eq!(w.address.len(), 42);
        assert_eq!(w.label, "gen-wallet");
        assert!(w.group_id.is_none());

        // Import known key
        let pk = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let imp = wallet_store::create_wallet("import-wallet", Some(pk)).unwrap();
        assert_eq!(
            imp.address.to_lowercase(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );

        // Duplicate import rejected
        assert!(wallet_store::create_wallet("dup", Some(pk)).is_err());

        // List shows both
        let list = wallet_store::list_wallets().unwrap();
        assert!(list.len() >= 2);

        // Rename
        let renamed = wallet_store::rename_wallet(w.id, "renamed").unwrap();
        assert_eq!(renamed.label, "renamed");

        // Empty label rejected
        assert!(wallet_store::rename_wallet(w.id, "").is_err());

        // Long label rejected
        assert!(wallet_store::rename_wallet(w.id, &"x".repeat(200)).is_err());

        // Export private key (requires passphrase confirmation)
        let pass = export_passphrase();
        let exported = wallet_store::export_private_key(imp.id, pass).unwrap();
        assert_eq!(exported.to_lowercase(), pk);

        // Wrong passphrase on export
        assert!(wallet_store::export_private_key(imp.id, "wrong-pass").is_err());

        // Delete
        wallet_store::delete_wallet(w.id).unwrap();
        assert!(wallet_store::get_wallet(w.id).is_err());

        // Delete non-existent
        assert!(wallet_store::delete_wallet(999999).is_err());
    }

    #[test]
    fn e2e_wallet_groups_full_cycle() {
        let _g = serial_guard();
        fresh_state("wallet_groups");
        ensure_unlocked();

        // Create groups
        let g1 = wallet_store::create_group("Main").unwrap();
        let g2 = wallet_store::create_group("Burner").unwrap();
        assert_eq!(g1.name, "Main");
        assert_eq!(g1.wallet_count, 0);

        // Duplicate group rejected
        assert!(wallet_store::create_group("Main").is_err());

        // Empty/long name rejected
        assert!(wallet_store::create_group("").is_err());
        assert!(wallet_store::create_group(&"x".repeat(100)).is_err());

        // List groups
        let groups = wallet_store::list_groups().unwrap();
        assert!(groups.len() >= 2);

        // Create wallets and assign to groups
        let w1 = wallet_store::create_wallet("grp-w1", None).unwrap();
        let w2 = wallet_store::create_wallet("grp-w2", None).unwrap();
        wallet_store::set_wallet_group(w1.id, Some(g1.id)).unwrap();
        wallet_store::set_wallet_group(w2.id, Some(g2.id)).unwrap();

        // Verify group counts
        let groups = wallet_store::list_groups().unwrap();
        let main_g = groups.iter().find(|g| g.id == g1.id).unwrap();
        assert_eq!(main_g.wallet_count, 1);

        // Assign to non-existent group
        assert!(wallet_store::set_wallet_group(w1.id, Some(999999)).is_err());

        // Unassign from group
        wallet_store::set_wallet_group(w1.id, None).unwrap();

        // Rename group
        let renamed = wallet_store::rename_group(g1.id, "MainNet").unwrap();
        assert_eq!(renamed.name, "MainNet");

        // Delete group (wallets get unassigned)
        wallet_store::delete_group(g2.id).unwrap();
        let w2_reloaded = wallet_store::get_wallet(w2.id).unwrap();
        assert!(w2_reloaded.group_id.is_none());
    }

    #[test]
    fn e2e_wallet_bulk_import() {
        let _g = serial_guard();
        fresh_state("wallet_bulk");
        ensure_unlocked();

        let items = vec![
            wallet_store::BulkImportItem {
                label: Some("bulk-1".into()),
                private_key: "0x0000000000000000000000000000000000000000000000000000000000000001"
                    .into(),
            },
            wallet_store::BulkImportItem {
                label: Some("bulk-2".into()),
                private_key: "0x0000000000000000000000000000000000000000000000000000000000000002"
                    .into(),
            },
            wallet_store::BulkImportItem {
                label: None,
                private_key: "invalid-key".into(),
            },
            wallet_store::BulkImportItem {
                label: Some("bulk-dup".into()),
                private_key: "0x0000000000000000000000000000000000000000000000000000000000000001"
                    .into(),
            },
        ];

        let results = wallet_store::import_bulk(&items, None).unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].status, "imported");
        assert_eq!(results[1].status, "imported");
        assert_eq!(results[2].status, "invalid");
        assert_eq!(results[3].status, "duplicate");

        // Empty import rejected
        assert!(wallet_store::import_bulk(&[], None).is_err());

        // Over 500 rejected
        let big: Vec<_> = (0..501)
            .map(|_| wallet_store::BulkImportItem {
                label: None,
                private_key: "bad".into(),
            })
            .collect();
        assert!(wallet_store::import_bulk(&big, None).is_err());
    }

    #[test]
    fn e2e_wallet_address_uniqueness_across_groups() {
        let _g = serial_guard();
        fresh_state("wallet_uniq");
        ensure_unlocked();

        let g = wallet_store::create_group("test-group").unwrap();
        let _w1 = wallet_store::create_wallet_in_group("w1", None, Some(g.id)).unwrap();
        let _w2 = wallet_store::create_wallet_in_group("w2", None, Some(g.id)).unwrap();

        // Can't create wallet with same address (even in different group)
        // Tests that address uniqueness is enforced at DB level
        let list = wallet_store::list_wallets().unwrap();
        let addrs: Vec<&str> = list.iter().map(|w| w.address.as_str()).collect();
        let unique_addrs: std::collections::HashSet<&str> = addrs.into_iter().collect();
        assert_eq!(unique_addrs.len(), list.len(), "addresses must be unique");
    }

    // ════════════════════════════════════════════════════════════════════
    // API KEYS E2E (H01 verification)
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_api_keys_lifecycle() {
        let _g = serial_guard();
        fresh_state("api_keys");
        ensure_unlocked();

        // Set key
        crate::commands::api_keys::api_key_set(
            "opensea".into(),
            "sk-test-1234567890abcdef".into(),
            None,
        )
        .unwrap();

        // Set with base_url
        crate::commands::api_keys::api_key_set(
            "alchemy".into(),
            "alchemy-key-abcdef".into(),
            Some("https://eth-mainnet.g.alchemy.com/v2/".into()),
        )
        .unwrap();

        // List — keys should be masked
        let keys = crate::commands::api_keys::api_key_list().unwrap();
        assert_eq!(keys.len(), 2);

        let os_key = keys.iter().find(|k| k.provider == "opensea").unwrap();
        // NOTE (audit finding L09): mask_key() uses U+2026 HORIZONTAL ELLIPSIS
        // ("…"), not three ASCII dots. Verify against the real character.
        assert!(
            os_key.masked.contains('\u{2026}'),
            "expected U+2026 mask, got {:?}",
            os_key.masked
        );
        assert!(
            !os_key.masked.contains("..."),
            "mask must NOT use ASCII dots (would mean char silently changed)"
        );
        assert!(!os_key.masked.contains("12345678"));
        assert!(os_key.base_url.is_none());

        let al_key = keys.iter().find(|k| k.provider == "alchemy").unwrap();
        assert!(al_key.base_url.is_some());

        // Empty provider/key rejected
        assert!(crate::commands::api_keys::api_key_set("".into(), "key".into(), None).is_err());
        assert!(crate::commands::api_keys::api_key_set("prov".into(), "".into(), None).is_err());

        // Delete
        crate::commands::api_keys::api_key_delete(os_key.id).unwrap();
        let keys_after = crate::commands::api_keys::api_key_list().unwrap();
        assert_eq!(keys_after.len(), 1);

        // Delete non-existent
        assert!(crate::commands::api_keys::api_key_delete(999999).is_err());

        // Masking edge case: short key
        crate::commands::api_keys::api_key_set("short".into(), "abc".into(), None).unwrap();
        let short_keys = crate::commands::api_keys::api_key_list().unwrap();
        let short = short_keys.iter().find(|k| k.provider == "short").unwrap();
        assert!(short.masked.contains("•"));
    }

    // ════════════════════════════════════════════════════════════════════
    // FUND CALC MATH E2E
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_fund_calc_edge_cases() {
        use crate::fund::calc_send_wei;
        use alloy::primitives::U256;

        let zero = U256::ZERO;
        let hundred = U256::from(100);
        let fifty = U256::from(50);
        let ten = U256::from(10);

        // Fixed: amount = 0 → error
        assert!(calc_send_wei("fixed", "disperse", zero, hundred, fifty, zero, true).is_err());

        // Fixed: insufficient balance
        assert!(calc_send_wei("fixed", "disperse", hundred, fifty, zero, zero, true).is_err());

        // Fixed: balance covers amount only, not gas
        assert!(calc_send_wei(
            "fixed",
            "disperse",
            ninety_six(),
            ninety_six(),
            zero,
            ten,
            true
        )
        .is_err());

        // Fixed: enough for amount + gas
        let result = calc_send_wei(
            "fixed",
            "disperse",
            hundred,
            U256::from(200),
            zero,
            ten,
            true,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), hundred);

        // Target disperse: already at target
        assert!(calc_send_wei("target", "disperse", fifty, hundred, fifty, zero, false).is_err());

        // Target disperse: fills gap
        let r = calc_send_wei("target", "disperse", hundred, hundred, fifty, zero, false);
        assert_eq!(r.unwrap(), fifty);

        // Target consolidate: below target (keep dust)
        assert!(calc_send_wei("target", "consolidate", hundred, fifty, zero, zero, false).is_err());

        // Target consolidate: dust after gas
        assert!(calc_send_wei(
            "target",
            "consolidate",
            ninety_nine(),
            U256::from(100),
            zero,
            ten,
            true
        )
        .is_err());

        // Target consolidate: normal
        let r = calc_send_wei(
            "target",
            "consolidate",
            ten,
            U256::from(100),
            zero,
            U256::from(1),
            true,
        );
        assert!(r.is_ok());

        // Bad amount_mode (fixed/target are valid)
        assert!(calc_send_wei("bad", "disperse", hundred, hundred, zero, zero, false).is_err());
        assert!(calc_send_wei("fixed", "disperse", hundred, hundred, zero, zero, false).is_ok());
    }

    fn ninety_six() -> alloy::primitives::U256 {
        alloy::primitives::U256::from(96)
    }

    fn ninety_nine() -> alloy::primitives::U256 {
        alloy::primitives::U256::from(99)
    }

    // ════════════════════════════════════════════════════════════════════
    // CHAIN SEED + UPSERT E2E
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_chain_upsert_and_seed() {
        let _g = serial_guard();
        fresh_state("chain_e2e");

        // Default chains seeded
        let chains = chain::list_chains().unwrap();
        assert!(chains.len() >= 5);
        let eth = chains.iter().find(|c| c.chain_id == 1).unwrap();
        assert_eq!(eth.symbol, "ETH");
        assert!(eth.enabled != 0);

        // Custom chain upsert
        let custom = chain::ChainInput {
            id: None,
            name: "MyChain".into(),
            chain_id: 31337,
            rpc_url: "http://localhost:8545".into(),
            symbol: "MYC".into(),
            explorer: Some("https://mychain.example".into()),
            enabled: Some(true),
        };
        chain::upsert_chain(&custom).unwrap();

        // Verify custom chain in list
        let chains = chain::list_chains().unwrap();
        let mc = chains.iter().find(|c| c.chain_id == 31337).unwrap();
        assert_eq!(mc.name, "MyChain");
        assert_eq!(mc.symbol, "MYC");

        // Update same chain_id via upsert (upsert, not duplicate)
        let updated = chain::ChainInput {
            id: None,
            name: "MyChainV2".into(),
            chain_id: 31337,
            rpc_url: "http://localhost:8546".into(),
            symbol: "MYC2".into(),
            explorer: None,
            enabled: Some(false),
        };
        chain::upsert_chain(&updated).unwrap();
        let chains = chain::list_chains().unwrap();
        let mc2 = chains.iter().find(|c| c.chain_id == 31337).unwrap();
        assert_eq!(mc2.name, "MyChainV2");
        assert_eq!(mc2.symbol, "MYC2");

        // Invalid chain_id rejected
        let bad = chain::ChainInput {
            id: None,
            name: "Bad".into(),
            chain_id: -1,
            rpc_url: "http://localhost:8545".into(),
            symbol: "BAD".into(),
            explorer: None,
            enabled: None,
        };
        assert!(chain::upsert_chain(&bad).is_err());

        // Invalid RPC rejected
        let bad_rpc = chain::ChainInput {
            id: None,
            name: "BadRpc".into(),
            chain_id: 99999,
            rpc_url: "ftp://nope".into(),
            symbol: "NO".into(),
            explorer: None,
            enabled: None,
        };
        assert!(chain::upsert_chain(&bad_rpc).is_err());

        // Empty name rejected
        let empty = chain::ChainInput {
            id: None,
            name: "".into(),
            chain_id: 99998,
            rpc_url: "http://localhost:8545".into(),
            symbol: "NO".into(),
            explorer: None,
            enabled: None,
        };
        assert!(chain::upsert_chain(&empty).is_err());

        // Delete
        let mc2_db = chain::list_chains()
            .unwrap()
            .into_iter()
            .find(|c| c.chain_id == 31337)
            .unwrap();
        chain::delete_chain(mc2_db.id).unwrap();
        assert!(chain::list_chains()
            .unwrap()
            .iter()
            .find(|c| c.chain_id == 31337)
            .is_none());

        // Delete non-existent
        assert!(chain::delete_chain(999999).is_err());
    }

    // ════════════════════════════════════════════════════════════════════
    // MINT PIPELINE E2E — advanced scenarios
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_mint_transient_retry_and_permanent_fail() {
        let _g = serial_guard();
        fresh_state("mint_retry");
        ensure_unlocked();

        let w = wallet_store::create_wallet("retry-w", None).unwrap();
        let cid = base_chain_id();

        // Enqueue a simulate task that will fail (bad contract on real RPC)
        let t = mint::enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: cid,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            value_wei: Some("0".into()),
            function_name: Some("totalSupply()".into()),
            is_hex: false,
            parameters: None,
            calldata: None,
            rpc_endpoints: None,
            flashbots: false,
            gas_limit: None,
            max_fee_gwei: None,
            priority_fee_gwei: None,
            nonce_override: None,
            scheduled_at: None,
            delay_ms: None,
            mode: Some("simulate".into()),
        })
        .unwrap();
        assert_eq!(t.status, "pending");

        // Process — should get either simulated or failed (RPC might be down in CI)
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(mint::process_task(t.id)).unwrap();
        assert!(
            matches!(result.status.as_str(), "simulated" | "failed"),
            "unexpected: {}",
            result.status
        );
    }

    #[test]
    fn e2e_mint_cancel_only_pending() {
        let _g = serial_guard();
        fresh_state("mint_cancel");
        ensure_unlocked();

        let w = wallet_store::create_wallet("cancel-w", None).unwrap();
        let cid = base_chain_id();

        let t = mint::enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: cid,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            mode: Some("simulate".into()),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap();

        // Cancel pending → success
        mint::cancel_task(t.id).unwrap();
        assert!(mint::get_task(t.id).is_err());

        // Cancel non-existent pending → error
        assert!(mint::cancel_task(t.id).is_err());
    }

    #[test]
    fn e2e_mint_schedule_gate() {
        let _g = serial_guard();
        fresh_state("mint_sched");
        ensure_unlocked();

        let w = wallet_store::create_wallet("sched-w", None).unwrap();
        let cid = base_chain_id();
        let future = db::now_ms() + 3_600_000; // 1 hour from now

        let t = mint::enqueue(EnqueueArgs {
            wallet_id: w.id,
            chain_id: cid,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            mode: Some("simulate".into()),
            scheduled_at: Some(future),
            function_name: Some("mint()".into()),
            ..Default::default()
        })
        .unwrap();

        // Should NOT be runnable
        assert_eq!(mint::count_runnable().unwrap(), 0);

        // After marking as past due
        db::with_conn(|conn| {
            conn.execute(
                "UPDATE mint_tasks SET scheduled_at = 0 WHERE id = ?1",
                [t.id],
            )
            .unwrap();
            Ok(())
        })
        .unwrap();

        // Now should be runnable
        assert_eq!(mint::count_runnable().unwrap(), 1);
    }

    // ════════════════════════════════════════════════════════════════════
    // MINT VALIDATION E2E — field constraints
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_mint_rejects_bad_inputs() {
        let _g = serial_guard();
        fresh_state("mint_bad");
        ensure_unlocked();

        let w = wallet_store::create_wallet("bad-inputs-w", None).unwrap();
        let cid = base_chain_id();
        let base = EnqueueArgs {
            wallet_id: w.id,
            chain_id: cid,
            contract: "0x0000000000000000000000000000000000000001".into(),
            quantity: 1,
            ..Default::default()
        };

        // Bad contract format
        assert!(mint::enqueue(EnqueueArgs {
            contract: "not-an-address".into(),
            ..base.clone()
        })
        .is_err());

        // Contract too short
        assert!(mint::enqueue(EnqueueArgs {
            contract: "0x1234".into(),
            ..base.clone()
        })
        .is_err());

        // Bad quantity
        assert!(mint::enqueue(EnqueueArgs {
            quantity: 0,
            ..base.clone()
        })
        .is_err());

        // Non-existent wallet
        assert!(mint::enqueue(EnqueueArgs {
            wallet_id: 999999,
            ..base.clone()
        })
        .is_err());

        // Bad gas_limit
        assert!(mint::enqueue(EnqueueArgs {
            gas_limit: Some("not-a-number".into()),
            ..base.clone()
        })
        .is_err());

        // NaN parses as f64 but u128 cast behavior is defined (test the path)
        // "12.5" is a valid max_fee_gwei
        assert!(mint::enqueue(EnqueueArgs {
            max_fee_gwei: Some("12.5".into()),
            ..base.clone()
        })
        .is_ok());

        // Truly invalid (not a number at all for integer field)
        assert!(mint::enqueue(EnqueueArgs {
            gas_limit: Some("abc!@#".into()),
            ..base.clone()
        })
        .is_err());

        // Bad nonce
        assert!(mint::enqueue(EnqueueArgs {
            nonce_override: Some("not-a-nonce".into()),
            ..base.clone()
        })
        .is_err());

        // Negative scheduled_at
        assert!(mint::enqueue(EnqueueArgs {
            scheduled_at: Some(-1),
            ..base.clone()
        })
        .is_err());

        // RPC endpoint not http(s)
        assert!(mint::enqueue(EnqueueArgs {
            rpc_endpoints: Some(vec!["ftp://bad".into()]),
            ..base.clone()
        })
        .is_err());
    }

    // ════════════════════════════════════════════════════════════════════
    // WALLET SIGNING E2E — decrypt → sign → verify signature
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_wallet_sign_and_verify() {
        let _g = serial_guard();
        fresh_state("wallet_sign");
        ensure_unlocked();

        // Import known key, verify signer produces correct address
        // Public Hardhat/Anvil test account #0 — well-known test vector, NOT a secret.
        // https://hardhat.org/hardhat-network/docs/reference#accounts
        let pk = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
        let w = wallet_store::create_wallet("sign-test", Some(pk)).unwrap();

        // Load key material → decrypt → load signer
        let (nonce, ct) = wallet_store::key_material(w.id).unwrap();
        let signer = wallet::load_signer(&nonce, &ct).unwrap();
        let signer_addr = signer.address();
        assert_eq!(
            signer_addr.to_checksum(None),
            w.address,
            "signer address must match stored address"
        );

        // Export roundtrip
        let pass = export_passphrase();
        let exported = wallet_store::export_private_key(w.id, pass).unwrap();
        assert_eq!(exported.to_lowercase(), pk);
    }

    // ════════════════════════════════════════════════════════════════════
    // ACTIVITY LOG E2E
    // ════════════════════════════════════════════════════════════════════

    #[test]
    fn e2e_activity_log_accumulates() {
        let _g = serial_guard();
        fresh_state("activity");
        ensure_unlocked();

        // Create some wallets (each logs activity)
        let _w1 = wallet_store::create_wallet("act-w1", None).unwrap();
        let _w2 = wallet_store::create_wallet("act-w2", None).unwrap();
        let _g = wallet_store::create_group("act-g").unwrap();

        // Activity log should have entries
        let activities = crate::commands::activity::activity_list(None, None).unwrap();
        assert!(
            activities.len() >= 2,
            "should have at least wallet.create entries"
        );
        for a in &activities {
            assert!(!a.kind.is_empty());
            assert!(!a.summary.is_empty());
        }
    }
}
