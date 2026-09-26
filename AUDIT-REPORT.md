# Aegis — Comprehensive Code Audit Report

**Repo:** `yaydyorhq/aegis`
**Stack:** Tauri 2 (Rust) + React 19 + TypeScript + Tailwind v4 + SQLite
**LOC:** 12,278 Rust + 7,167 TypeScript = **19,445 total**
**Date:** 2026-09-26

---

## Executive Summary

Aegis is a **well-architected, security-conscious desktop wallet vault and Web3 command center**. The crypto primitives are correctly implemented, private keys never leave the device, and the IPC boundary is clean. There are no critical security vulnerabilities.

The issues found are **functional bugs, UX gaps, and minor code quality issues** — no data leaks, no auth bypasses, no key exposure.

**Severity legend:**
- 🔴 Critical (security/data loss)
- 🟠 High (functional bug affecting users)
- 🟡 Medium (edge case / UX problem)
- 🟢 Low (code quality / cosmetic)

---

## 🔴 Critical Issues

**None found.** The security architecture is solid.

---

## 🟠 High-Severity Issues

### H01: `api_key_list()` decrypts keys then discards plaintext — memory exposure window
**File:** `src-tauri/src/commands/api_keys.rs:70`
**Issue:** `api_key_list()` decrypts every API key in the DB to compute the masked display string. The decrypted plaintext lives on the heap as a `String` (not `Zeroizing<String>`) until it's dropped. While masked, the full key exists in memory temporarily.

```rust
let plain = vault::decrypt(&nonce, &ct).unwrap_or_default();
let key = String::from_utf8_lossy(&plain).to_string();
out.push(ApiKeyRow { ... masked: mask_key(&key) ... });
// key stays in memory until function return
```

**Impact:** Low in practice (local desktop app), but violates the "secrets never in memory longer than needed" principle.
**Fix:** Compute mask directly from ciphertext or use `Zeroizing` for the intermediate.

### H02: Argon2id DEK derivation uses truncated hash — not standard KDF
**File:** `src-tauri/src/vault/mod.rs:48-61`
**Issue:** `derive_dek` uses `Argon2::default()` (Argon2id) which is good, but then **truncates the output to 32 bytes**:

```rust
let raw = hash_bytes.as_bytes();
let mut dek = Zeroizing::new([0u8; 32]);
let n = raw.len().min(32);
dek[..n].copy_from_slice(&raw[..n]);
```

`hash_password` returns a PHC string with embedded parameters, and `hash_bytes` returns the raw hash output. Argon2id default output is 32 bytes so this happens to work correctly. However, using `Argon2::default().hash_password()` and extracting `.hash()` is roundabout. Direct use of `argon2::Argon2::new().hash_password_into()` would be cleaner and avoid the PHC string overhead.

**Impact:** Functional, not a security bug — default output is exactly 32 bytes.
**Severity downgraded to 🟡** after review.

### H03: `ensure_column` SQL injection via table/column names
**File:** `src-tauri/src/db/mod.rs:82-85`
**Issue:**
```rust
conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {name} {decl}"), [])
```

Table and column names are interpolated directly into SQL via `format!`. While all callers pass hardcoded strings (`"wallets"`, `"group_id"`, etc.), this pattern is dangerous if future code passes user input.

**Impact:** No current vulnerability (all callers are hardcoded). Risk is future regressions.
**Fix:** Validate table/column names against an allowlist, or use `quote_ident()`.

---

## 🟡 Medium-Severity Issues

### M01: `delay_ms` cast truncates i64 to u64
**File:** `src-tauri/src/mint.rs:884`
```rust
tokio::time::sleep(std::time::Duration::from_millis(task.delay_ms as u64)).await;
```
`delay_ms` is `i64`. If negative (shouldn't happen due to validation, but `recover_stale` could theoretically set bad values), this wraps to a very large u64. The `max(0)` on line 380 handles enqueue, but recovery code doesn't re-validate.

### M02: No rate limiting on vault unlock attempts
**File:** `src-tauri/src/vault/mod.rs:78`
`unlock_or_create` has no attempt counter or cooldown. An attacker with local filesystem access could brute-force the passphrase. Argon2id default parameters provide some protection, but a dedicated attempt limiter would be better.

### M03: `spam` mode has no throttle — rapid broadcast flood
**File:** `src-tauri/src/mint.rs:195`
The `spam` mode allows rapid-fire re-signing without any delay between attempts. Combined with the 250ms scheduler tick, this could produce excessive RPC calls.

### M04: Frontend localStorage used for UI preferences only (SAFE)
**File:** `src/pages/Pnl.tsx`, `src/pages/NftChecker.tsx`
localStorage stores: `pnl.collectionContract`, `pnl.collectionWindowBlocks.v2`, `pnl.collectionFeePct`, `pnl.windowBlocks`, `pnl.tab`, `nft.windowBlocks.v2`. **No secrets stored in localStorage.** This is correct behavior.

### M05: `api_key_list()` returns `unwrap_or_default()` on decrypt failure
**File:** `src-tauri/src/commands/api_keys.rs:70`
```rust
let plain = vault::decrypt(&nonce, &ct).unwrap_or_default();
```
If decrypt fails (e.g. wrong passphrase still in vault, DB corruption), the key silently returns `""` which masks as `""` → no error shown to user. Should surface the error.

### M06: No `vacuum` or DB size management
The SQLite DB grows unboundedly. Activity log, nft_cache, pnl_scans, and collection_pnl_scans accumulate forever. No cleanup/pruning mechanism exists. For a local app with years of use, this could become significant.

### M07: `fund.rs` releases runner via both `release_runner()` AND `RunGuard::drop()`
**File:** `src-tauri/src/fund.rs:16,26`
The `release_runner()` function sets the flag to false, but `RunGuard::drop()` does the same. If the guard was created, `release_runner()` is a redundant write. If the guard was NOT created (error path), `release_runner()` should be the only release path. This double-release pattern is confusing but not buggy — both writes are idempotent.

---

## 🟢 Low-Severity / Code Quality Issues

### L01: 126 `.unwrap()` calls across Rust codebase
Most are in `fund.rs` (38), `mint.rs` (34), and `db/mod.rs` (19). Many are in error handling paths where unwrap is acceptable (e.g. `Mutex::lock().unwrap()` on non-poisonable locks, or in `#[cfg(test)]` blocks). However, some are in production code paths:

- `fund.rs:197`: `serde_json::from_str(&peers_json).unwrap_or_default()` — safe (default), but could log corruption.
- `db/mod.rs:154`: `now_ms().unwrap_or(0)` — clock error returns epoch 0, which would make all scheduled tasks appear overdue.

### L02: `AppShell.tsx` and `TopBar.tsx` are thin wrappers
Only 46 and 67 LOC respectively. Could be inlined into the layout, but this is a style preference — not a bug.

### L03: `vite.config.ts` has hardcoded `localhost:1420` dev URL
Standard for Tauri dev. Not an issue.

### L04: No `#[cfg(test)]` tests for fund.rs, opensea.rs, collection_pnl.rs
Only `vault/mod.rs`, `wallet/mod.rs`, `db/mod.rs`, and `selectors_dbg_test.rs` have tests. The largest files (fund 1600 LOC, opensea 1383 LOC, collection_pnl 1500 LOC) have **zero tests**.

### L05: CSP in tauri.conf.json is permissive for network
```
connect-src 'self' ipc: http://ipc.localhost https: http:;
```
Allows HTTP/HTTPS to any destination. This is necessary for the RPC/OpenSea functionality but means the app can reach any network endpoint. For a wallet tool, this is expected.

### L06: `Cargo.toml` has `alloy = "2"` with `features = ["full"]`
The `full` feature pulls in a large dependency tree. For the subset of alloy actually used (primitives, signers, consensus), a more targeted feature set would reduce compile time and binary size.

### L07: `tsc --noEmit` passes clean ✅
### L08: `cargo check` — pending (deps too large for full build on this VPS without Tauri/GTK)

---

## Architecture Assessment

### Strengths
1. **Clean vault architecture** — Argon2id + AES-256-GCM, Zeroizing key material, verifier pattern
2. **Single DB mutex** — prevents SQLite concurrency issues
3. **Run guard pattern** — prevents duplicate mint runners
4. **Per-wallet sequential lanes** — prevents nonce collisions during parallel FCFS
5. **Transient error retry** — one auto-requeue for transport failures
6. **Chain ID verification** — concurrent chain_id check prevents wrong-network signing
7. **Scheduled task recovery** — `recover_stale_tasks` handles crashes
8. **Clean IPC boundary** — single `ipc()` helper, no secrets in frontend state
9. **DB migration pattern** — `ensure_column` for backward compatibility
10. **Configurable RPC endpoints** — per-task fallback list

### Weaknesses
1. **No test coverage on critical paths** — fund transfer, OpenSea integration, collection PnL
2. **No DB cleanup** — activity/nft_cache/pnl grow forever
3. **No passphrase attempt limiting** — brute-force possible with filesystem access
4. **`alloy = "full"`** — massive dependency for limited feature usage
5. **Double release pattern in fund.rs** — confusing but not buggy

---

## Verdict

**Code quality: 8/10** for a personal/small-team desktop app. The security-critical paths (vault, key encryption, signing) are well-designed. The main areas for improvement are test coverage and DB lifecycle management.

**No blockers for personal use.** If this becomes a shared/published tool, address H01, M02, M06, and add tests for fund/opensea/pnl.
