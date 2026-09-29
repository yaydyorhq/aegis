//! Cross-module nonce coordination for one wallet on one chain.
//!
//! Mint lanes and the fund runner are separate tasks that each used to read
//! `eth_getTransactionCount("pending")` and sign immediately. When both ran
//! for the same wallet they read the same number and both signed it — the
//! second tx then either replaced the first or died with "nonce too low".
//!
//! This keeps a process-local high-water mark per (wallet, chain):
//!
//! * `reserve` returns the next free nonce — at least the chain's own
//!   `pending` count, and never one already handed out here.
//! * `release` hands a reservation back when the tx provably never reached a
//!   mempool (signing failed, every endpoint rejected the send) so the next
//!   attempt does not leave a permanent gap that would stall the queue.
//!
//! Known limit: the mark lives in memory, so a restart re-seeds from the
//! chain's `pending` count. Nodes that never saw our broadcast would report
//! a stale number; the reading endpoint is normally the one we broadcast to,
//! which already has the tx in its pool.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

type Key = (i64, i64); // (wallet_id, chain_id)

static NEXT: Lazy<Mutex<HashMap<Key, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));

fn lock_unpoisoned() -> MutexGuard<'static, HashMap<Key, u64>> {
    NEXT.lock().unwrap_or_else(|e| e.into_inner())
}

/// Next nonce for `wallet_id` on `chain_id`.
///
/// `chain_nonce` is the `pending` count the caller just read (used to seed and
/// to adopt progress made outside this process). `override_nonce` bypasses the
/// high-water mark but still advances it, so later auto-reserves cannot collide.
pub fn reserve(wallet_id: i64, chain_id: i64, chain_nonce: u64, override_nonce: Option<u64>) -> u64 {
    let mut map = lock_unpoisoned();
    let key = (wallet_id, chain_id);
    let local = map.entry(key).or_insert(chain_nonce);
    if *local < chain_nonce {
        // Landed elsewhere (another process, an external tx) — adopt it.
        *local = chain_nonce;
    }
    match override_nonce {
        Some(n) => {
            if *local <= n {
                *local = n + 1;
            }
            n
        }
        None => {
            let chosen = *local;
            *local = chosen + 1;
            chosen
        }
    }
}

/// Give back a reservation that never made it to a mempool.
///
/// Only rewinds when `nonce` is the most recent hand-out — a later reservation
/// may already depend on it being consumed.
pub fn release(wallet_id: i64, chain_id: i64, nonce: u64) {
    let mut map = lock_unpoisoned();
    if let Some(local) = map.get_mut(&(wallet_id, chain_id)) {
        if *local == nonce + 1 {
            *local = nonce;
        }
    }
}

#[cfg(test)]
pub fn reset_for_tests(wallet_id: i64, chain_id: i64) {
    lock_unpoisoned().remove(&(wallet_id, chain_id));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (i64, i64) {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as i64;
        (n, n + 1)
    }

    #[test]
    fn consecutive_reserves_never_collide() {
        let (w, c) = ids();
        assert_eq!(reserve(w, c, 7, None), 7);
        assert_eq!(reserve(w, c, 7, None), 8); // chain still reports 7
        assert_eq!(reserve(w, c, 7, None), 9);
        // Our mark is ahead of a lagging chain read: never go backwards.
        assert_eq!(reserve(w, c, 9, None), 10);
        // Progress made outside this process is adopted.
        assert_eq!(reserve(w, c, 12, None), 12);
        assert_eq!(reserve(w, c, 12, None), 13);
        reset_for_tests(w, c);
    }

    #[test]
    fn release_rewinds_only_the_top_reservation() {
        let (w, c) = ids();
        let a = reserve(w, c, 3, None);
        let b = reserve(w, c, 3, None);
        assert_eq!((a, b), (3, 4));
        // 4 is outstanding, so 3 must NOT rewind — the next hand-out stays 5.
        release(w, c, a);
        assert_eq!(reserve(w, c, 3, None), 5);
        reset_for_tests(w, c);
    }

    #[test]
    fn release_rewinds_when_it_is_the_top() {
        let (w, c) = ids();
        let a = reserve(w, c, 3, None);
        assert_eq!(a, 3);
        release(w, c, a);
        assert_eq!(reserve(w, c, 3, None), 3);
        reset_for_tests(w, c);
    }

    #[test]
    fn override_wins_but_blocks_later_auto_reserves() {
        let (w, c) = ids();
        assert_eq!(reserve(w, c, 5, Some(9)), 9);
        assert_eq!(reserve(w, c, 5, None), 10);
        reset_for_tests(w, c);
    }

    #[test]
    fn external_progress_adopts_higher_chain_nonce() {
        let (w, c) = ids();
        assert_eq!(reserve(w, c, 2, None), 2);
        // Something outside the app consumed 2 and 3.
        assert_eq!(reserve(w, c, 4, None), 4);
        reset_for_tests(w, c);
    }
}
