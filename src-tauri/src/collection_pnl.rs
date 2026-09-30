use crate::chain;
use crate::chain::eligibility;
use crate::chain::nft::{parse_erc721_logs, ERC721_TRANSFER_TOPIC, OwnershipEvent};
use crate::error::{AppError, AppResult};
use crate::opensea::OpenSeaClient;
use crate::wallet_store;
use alloy::primitives::U256;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// keccak256("OrderFulfilled(bytes32,address,address,address,(uint8,address,uint256,uint256)[],(uint8,address,uint256,uint256,address)[])")
/// — Seaport 1.5/1.6 canonical event (verified against 31 mainnet logs).
pub const ORDER_FULFILLED_TOPIC: &str =
    "0x9d9af8e38d66c62e2c12f0225249fd9d721c54b83f48d9352c97c6cacdcb6f31";

#[cfg(test)]
const ORDER_FULFILLED_SIG: &str = "OrderFulfilled(bytes32,address,address,address,(uint8,address,uint256,uint256)[],(uint8,address,uint256,uint256,address)[])";

const ZERO_ADDR: &str = "0x0000000000000000000000000000000000000000";
const MAX_TXS: usize = 2_000;
const TX_CONCURRENCY: usize = 6;
/// Seaport exchange addresses on this chain (1.6 is the active one; 1.5 has
/// zero OrderFulfilled logs but is queried anyway for safety).
const SEAPORT_16: &str = "0x0000000000000068f116a894984e2db1123eb395";
const SEAPORT_15: &str = "0x00000000000000adc04c56bf30ac9d3c0aaf14dc";

// ───────────────────────────── public types ─────────────────────────────

#[derive(Clone, Debug)]
pub struct Target {
    pub wallet_id: Option<i64>,
    pub label: String,
    pub address: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CollectionPnlRow {
    pub wallet_id: Option<i64>,
    pub label: String,
    pub address: String,
    pub minted: u64,
    pub bought: u64,
    pub sold: u64,
    /// Tokens received inside the window that are still held (cost-basis covered).
    pub holding: u64,
    /// Current on-chain balance (balanceOf), fallback = window holding count.
    pub balance: u64,
    pub balance_known: bool,
    /// Holdings that predate the window (no cost basis tracked) — excluded from unrealized.
    pub out_of_window: u64,
    pub spent_eth: String,
    pub gas_eth: String,
    /// Signed: "+1.5" / "-0.25" / "0".
    pub realized_eth: String,
    pub unrealized_eth: Option<String>,
    pub net_eth: Option<String>,
    pub roi_pct: Option<f64>,
    pub basis_incomplete: bool,
    pub unpriced_sales: u64,
    pub transfer_in: u64,
    pub transfer_out: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct CollectionPnlTotals {
    pub wallets: u64,
    pub minted: u64,
    pub bought: u64,
    pub sold: u64,
    pub holding: u64,
    pub balance: u64,
    pub spent_eth: String,
    pub gas_eth: String,
    pub realized_eth: String,
    pub unrealized_eth: Option<String>,
    pub net_eth: Option<String>,
    pub roi_pct: Option<f64>,
    pub basis_incomplete: bool,
    pub unpriced_sales: u64,
}

#[derive(Serialize, Deserialize)]
pub struct CollectionPnlResult {
    pub contract: String,
    pub chain_id: i64,
    pub native_symbol: String,
    pub window_blocks: u64,
    pub from_block: u64,
    pub to_block: u64,
    pub floor_eth: Option<String>,
    /// "opensea" | "seaport-sales" | "window-sales" | None.
    pub floor_source: Option<String>,
    pub fee_bps: u64,
    pub rows: Vec<CollectionPnlRow>,
    pub totals: CollectionPnlTotals,
    pub warnings: Vec<String>,
    pub scanned_at: i64,
}

// ───────────────────────────── Seaport decoder ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct OrderItem {
    pub item_type: u8,
    pub token: String,
    pub identifier: U256,
    pub amount: U256,
    pub recipient: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FulfilledOrder {
    pub offer: Vec<OrderItem>,
    pub consideration: Vec<OrderItem>,
    /// Σ native consideration (what the buyer pays on an ETH listing fulfilment).
    pub native_total: U256,
}

fn read_u256(data: &[u8], byte_off: usize) -> Option<U256> {
    let end = byte_off.checked_add(32)?;
    if end > data.len() {
        return None;
    }
    Some(U256::from_be_slice(&data[byte_off..end]))
}

fn read_u8(data: &[u8], byte_off: usize) -> Option<u8> {
    let w = read_u256(data, byte_off)?;
    if w > U256::from(u8::MAX) {
        return None;
    }
    Some(w.as_limbs()[0] as u8)
}

fn read_addr(data: &[u8], byte_off: usize) -> Option<String> {
    let end = byte_off.checked_add(32)?;
    if end > data.len() {
        return None;
    }
    Some(format!("0x{}", hex::encode(&data[byte_off + 12..end])))
}

fn u256_to_usize(v: U256) -> Option<usize> {
    usize::try_from(v).ok()
}

fn array_len(data: &[u8], off: usize, item_words: usize) -> Option<usize> {
    let n = u256_to_usize(read_u256(data, off)?)?;
    if n > 128 {
        return None;
    }
    let items_bytes = n.checked_mul(item_words * 32)?;
    let end = off.checked_add(32)?.checked_add(items_bytes)?;
    if end > data.len() {
        return None;
    }
    Some(n)
}

fn decode_items(data: &[u8], off: usize, item_words: usize, with_recipient: bool) -> Option<Vec<OrderItem>> {
    let n = array_len(data, off, item_words)?;
    let mut out = Vec::with_capacity(n);
    let mut pos = off.checked_add(32)?;
    for _ in 0..n {
        let item_type = read_u8(data, pos)?;
        let token = read_addr(data, pos + 32)?;
        let identifier = read_u256(data, pos + 64)?;
        let amount = read_u256(data, pos + 96)?;
        let recipient = if with_recipient {
            Some(read_addr(data, pos + 128)?)
        } else {
            None
        };
        out.push(OrderItem {
            item_type,
            token,
            identifier,
            amount,
            recipient,
        });
        pos = pos.checked_add(item_words * 32)?;
    }
    Some(out)
}

/// Head-4 (Seaport 1.5/1.6): word0 orderHash, word1 extra address,
/// word2 offer offset, word3 consideration offset.
/// Head-3 (older): word0 orderHash, word1 offer offset, word2 consideration offset.
/// Both offsets are validated against the array length word before use.
fn decode_with_head(data: &[u8], head: usize) -> Option<FulfilledOrder> {
    let head_bytes = head * 32;
    if data.len() < head_bytes {
        return None;
    }
    let offer_off = u256_to_usize(read_u256(data, (head - 2) * 32)?)?;
    let cons_off = u256_to_usize(read_u256(data, (head - 1) * 32)?)?;
    if offer_off < head_bytes || cons_off < head_bytes {
        return None;
    }
    let offer = decode_items(data, offer_off, 4, false)?;
    let consideration = decode_items(data, cons_off, 5, true)?;
    let native_total = consideration
        .iter()
        .filter(|i| i.item_type == 0)
        .fold(U256::ZERO, |a, b| a.saturating_add(b.amount));
    Some(FulfilledOrder {
        offer,
        consideration,
        native_total,
    })
}

pub fn decode_order_fulfilled(log: &serde_json::Value) -> Option<FulfilledOrder> {
    let topics = log.get("topics")?.as_array()?;
    if topics.len() < 3 {
        return None;
    }
    if topics[0].as_str()? != ORDER_FULFILLED_TOPIC {
        return None;
    }
    let data_hex = log.get("data")?.as_str()?;
    let data = hex::decode(data_hex.trim_start_matches("0x")).ok()?;
    decode_with_head(&data, 4).or_else(|| decode_with_head(&data, 3))
}

fn order_covers(o: &FulfilledOrder, contract: &str, token_id: &str) -> bool {
    let Ok(id) = U256::from_str_radix(token_id, 10) else {
        return false;
    };
    o.offer
        .iter()
        .chain(o.consideration.iter())
        .any(|i| {
            (i.item_type == 2 || i.item_type == 3)
                && i.token.eq_ignore_ascii_case(contract)
                && i.identifier == id
        })
}

fn order_has_erc20(o: &FulfilledOrder) -> bool {
    o.offer
        .iter()
        .chain(o.consideration.iter())
        .any(|i| i.item_type == 1 && !i.amount.is_zero())
}

/// Native proceeds actually paid to `wallet` inside this order.
fn consideration_native_to(o: &FulfilledOrder, wallet: &str) -> U256 {
    o.consideration
        .iter()
        .filter(|i| {
            i.item_type == 0 && i.recipient.as_deref().map(|r| r.eq_ignore_ascii_case(wallet)) == Some(true)
        })
        .fold(U256::ZERO, |a, b| a.saturating_add(b.amount))
}

/// Gross per-token price paid in this order (all-in, incl. fees):
/// listing fulfilment → native consideration / NFTs offered;
/// bid acceptance → native offered / NFTs in consideration.
/// None when no native price is attached to `contract`'s tokens.
fn order_unit_price(o: &FulfilledOrder, contract: &str) -> Option<U256> {
    let count_nfts = |items: &[OrderItem]| -> u64 {
        items
            .iter()
            .filter(|i| (i.item_type == 2 || i.item_type == 3) && i.token.eq_ignore_ascii_case(contract))
            .count() as u64
    };
    let offer_nfts = count_nfts(&o.offer);
    if offer_nfts > 0 && !o.native_total.is_zero() {
        return Some(o.native_total / U256::from(offer_nfts));
    }
    let cons_nfts = count_nfts(&o.consideration);
    if cons_nfts > 0 {
        let native_offer = o
            .offer
            .iter()
            .filter(|i| i.item_type == 0)
            .fold(U256::ZERO, |a, b| a.saturating_add(b.amount));
        if !native_offer.is_zero() {
            return Some(native_offer / U256::from(cons_nfts));
        }
    }
    None
}

// ───────────────────────────── accounting ─────────────────────────────

#[derive(Clone, Debug)]
struct SaleRec {
    token: String,
    price: U256,
    block: u64,
    log_index: u64,
}

#[derive(Default)]
struct WalletAcc {
    minted: u64,
    bought: u64,
    sold: u64,
    transfer_in: u64,
    transfer_out: u64,
    unpriced_sales: u64,
    spent: U256,
    gas: U256,
    realized: i128,
    held: HashSet<String>,
    basis: HashMap<String, U256>,
    basis_unknown: bool,
    sales: Vec<SaleRec>,
}

struct TxMeta {
    from: String,
    value: U256,
    gas_cost: U256,
    orders: Vec<FulfilledOrder>,
}

fn u256_to_u128(v: U256) -> u128 {
    u128::try_from(v).unwrap_or(u128::MAX)
}

/// Pure per-wallet accounting over chronologically sorted events.
/// Cost basis rules (in order): Seaport order w/ native → buyer cost;
/// tx.value sent by the wallet → direct payment; free mint → 0;
/// ERC-20-only order → unknown basis flag; otherwise → gift (basis 0).
/// Disposals: order w/ native to wallet → priced sale; otherwise unpriced;
/// no order → plain transfer (basis dropped).
fn account_wallet(
    wallet: &str,
    events: &[OwnershipEvent],
    txs: &HashMap<String, TxMeta>,
    acq: &HashMap<String, u64>,
    outc: &HashMap<String, u64>,
    contract: &str,
) -> WalletAcc {
    let mut acc = WalletAcc::default();
    let mut gas_seen: HashSet<&str> = HashSet::new();
    for e in events {
        if let Some(tx) = txs.get(&e.tx_hash) {
            if tx.from == wallet && gas_seen.insert(e.tx_hash.as_str()) {
                acc.gas = acc.gas.saturating_add(tx.gas_cost);
            }
        }
        let is_in = e.to == wallet && e.from != wallet;
        let is_out = e.from == wallet && e.to != wallet;
        if !is_in && !is_out {
            continue;
        }
        let tx = txs.get(&e.tx_hash);
        let order = tx.and_then(|t| t.orders.iter().find(|o| order_covers(o, contract, &e.token_id)));
        let token = e.token_id.clone();

        if is_in {
            let n = acq.get(&e.tx_hash).copied().unwrap_or(1).max(1) as u128;
            let (kind, cost, unknown) = if let Some(o) = order {
                if !o.native_total.is_zero() {
                    ("bought", o.native_total / U256::from(n), false)
                } else if order_has_erc20(o) {
                    ("bought", U256::ZERO, true)
                } else {
                    ("minted", U256::ZERO, false)
                }
            } else {
                let value = tx.map(|t| t.value).unwrap_or(U256::ZERO);
                let by_self = tx.map(|t| t.from == wallet).unwrap_or(false);
                if !value.is_zero() && by_self {
                    let c = value / U256::from(n);
                    if e.from == ZERO_ADDR {
                        ("minted", c, false)
                    } else {
                        ("bought", c, false)
                    }
                } else if e.from == ZERO_ADDR {
                    ("minted", U256::ZERO, false)
                } else {
                    ("gift", U256::ZERO, false)
                }
            };
            match kind {
                "bought" => acc.bought += 1,
                "minted" => acc.minted += 1,
                _ => acc.transfer_in += 1,
            }
            acc.spent = acc.spent.saturating_add(cost);
            if unknown {
                acc.basis_unknown = true;
            }
            acc.basis.insert(token.clone(), cost);
            acc.held.insert(token.clone());
        } else {
            let n = outc.get(&e.tx_hash).copied().unwrap_or(1).max(1) as u128;
            let basis = acc.basis.remove(&token).unwrap_or(U256::ZERO);
            match order {
                Some(o) => {
                    let proceeds = consideration_native_to(o, wallet) / U256::from(n);
                    if proceeds.is_zero() {
                        acc.unpriced_sales += 1;
                    } else {
                        acc.sold += 1;
                        let p = u256_to_u128(proceeds) as i128;
                        let b = u256_to_u128(basis) as i128;
                        acc.realized = acc.realized.saturating_add(p - b);
                        acc.sales.push(SaleRec {
                            token: token.clone(),
                            price: proceeds,
                            block: e.block_number,
                            log_index: e.log_index,
                        });
                    }
                }
                None => acc.transfer_out += 1,
            }
            acc.held.remove(&token);
        }
    }
    acc
}

// ───────────────────────────── row / totals ─────────────────────────────

struct RowNums {
    minted: u64,
    bought: u64,
    sold: u64,
    holding: u64,
    balance: u64,
    spent: U256,
    gas: U256,
    realized: i128,
    unrealized: Option<i128>,
    net: Option<i128>,
    denom: U256,
    unpriced: u64,
    basis_incomplete: bool,
}

fn fmt_signed(v: i128) -> String {
    if v > 0 {
        format!("+{}", crate::pnl::format_units(U256::from(v as u128), 18))
    } else if v < 0 {
        format!("-{}", crate::pnl::format_units(U256::from(v.unsigned_abs()), 18))
    } else {
        "0".into()
    }
}

fn build_row(
    t: &Target,
    acc: &WalletAcc,
    balance: Option<u64>,
    floor_net: Option<U256>,
) -> (CollectionPnlRow, RowNums) {
    let holding = acc.held.len() as u64;
    let balance_val = balance.unwrap_or(holding);
    let out_of_window = balance_val.saturating_sub(holding);
    let mut basis_incomplete = acc.basis_unknown;
    match balance {
        Some(b) if b == holding => {}
        _ => basis_incomplete = true,
    }

    let held_basis = acc
        .held
        .iter()
        .filter_map(|tok| acc.basis.get(tok))
        .fold(U256::ZERO, |a, b| a.saturating_add(*b));

    // Unrealized covers in-window tokens only: pre-window holdings have no
    // tracked cost basis and would otherwise show up as pure profit.
    let unrealized = floor_net.map(|f| {
        let value = u256_to_u128(f.saturating_mul(U256::from(holding))) as i128;
        value - u256_to_u128(held_basis) as i128
    });
    let net = unrealized.map(|u| {
        acc.realized
            .saturating_add(u)
            .saturating_sub(u256_to_u128(acc.gas) as i128)
    });
    let denom = acc.spent.saturating_add(acc.gas);
    let roi_pct = match (net, denom.is_zero()) {
        (Some(n), false) => Some((n as f64 / u256_to_u128(denom) as f64) * 100.0),
        _ => None,
    };

    let nums = RowNums {
        minted: acc.minted,
        bought: acc.bought,
        sold: acc.sold,
        holding,
        balance: balance_val,
        spent: acc.spent,
        gas: acc.gas,
        realized: acc.realized,
        unrealized,
        net,
        denom,
        unpriced: acc.unpriced_sales,
        basis_incomplete,
    };
    let row = CollectionPnlRow {
        wallet_id: t.wallet_id,
        label: t.label.clone(),
        address: t.address.clone(),
        minted: acc.minted,
        bought: acc.bought,
        sold: acc.sold,
        holding,
        balance: balance_val,
        balance_known: balance.is_some(),
        out_of_window,
        spent_eth: crate::pnl::format_units(acc.spent, 18),
        gas_eth: crate::pnl::format_units(acc.gas, 18),
        realized_eth: fmt_signed(acc.realized),
        unrealized_eth: unrealized.map(fmt_signed),
        net_eth: net.map(fmt_signed),
        roi_pct,
        basis_incomplete,
        unpriced_sales: acc.unpriced_sales,
        transfer_in: acc.transfer_in,
        transfer_out: acc.transfer_out,
    };
    (row, nums)
}

#[derive(Default)]
struct TotalsNum {
    wallets: u64,
    minted: u64,
    bought: u64,
    sold: u64,
    holding: u64,
    balance: u64,
    spent: U256,
    gas: U256,
    realized: i128,
    unrealized: i128,
    net: i128,
    denom: U256,
    unrealized_known: bool,
    net_known: bool,
    unpriced: u64,
    basis_incomplete: bool,
}

impl TotalsNum {
    fn new() -> Self {
        Self {
            unrealized_known: true,
            net_known: true,
            ..Default::default()
        }
    }

    fn add(&mut self, n: &RowNums) {
        self.wallets += 1;
        self.minted += n.minted;
        self.bought += n.bought;
        self.sold += n.sold;
        self.holding += n.holding;
        self.balance += n.balance;
        self.spent = self.spent.saturating_add(n.spent);
        self.gas = self.gas.saturating_add(n.gas);
        self.realized = self.realized.saturating_add(n.realized);
        self.denom = self.denom.saturating_add(n.denom);
        self.unpriced += n.unpriced;
        self.basis_incomplete |= n.basis_incomplete;
        match n.unrealized {
            Some(u) => self.unrealized = self.unrealized.saturating_add(u),
            None => self.unrealized_known = false,
        }
        match n.net {
            Some(v) => self.net = self.net.saturating_add(v),
            None => self.net_known = false,
        }
    }

    fn finish(&self) -> CollectionPnlTotals {
        let unrealized = if self.unrealized_known {
            Some(fmt_signed(self.unrealized))
        } else {
            None
        };
        let net_val = self.net_known.then_some(self.net);
        let roi_pct = match (net_val, self.denom.is_zero()) {
            (Some(n), false) => Some((n as f64 / u256_to_u128(self.denom) as f64) * 100.0),
            _ => None,
        };
        CollectionPnlTotals {
            wallets: self.wallets,
            minted: self.minted,
            bought: self.bought,
            sold: self.sold,
            holding: self.holding,
            balance: self.balance,
            spent_eth: crate::pnl::format_units(self.spent, 18),
            gas_eth: crate::pnl::format_units(self.gas, 18),
            realized_eth: fmt_signed(self.realized),
            unrealized_eth: unrealized,
            net_eth: net_val.map(fmt_signed),
            roi_pct,
            basis_incomplete: self.basis_incomplete,
            unpriced_sales: self.unpriced,
        }
    }
}

// ───────────────────────────── scan ─────────────────────────────

fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn addr_topic(address: &str) -> String {
    let a = address.trim().trim_start_matches("0x").to_lowercase();
    format!("0x{:0>64}", a)
}

pub fn resolve_targets(
    wallet_ids: Option<Vec<i64>>,
    extra_addresses: Option<Vec<String>>,
) -> AppResult<Vec<Target>> {
    let mut out: Vec<Target> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let rows = match wallet_ids {
        Some(ids) if !ids.is_empty() => {
            let mut v = Vec::new();
            for id in ids {
                v.push(wallet_store::get_wallet(id)?);
            }
            v
        }
        _ => wallet_store::list_wallets()?,
    };
    for w in rows {
        let lc = w.address.to_lowercase();
        if is_address(&lc) && seen.insert(lc) {
            out.push(Target {
                wallet_id: Some(w.id),
                label: w.label,
                address: w.address,
            });
        }
    }
    if let Some(extra) = extra_addresses {
        for a in extra {
            let a = a.trim().to_lowercase();
            if a.is_empty() || !is_address(&a) {
                return Err(AppError::Invalid(format!("bad address: {a}")));
            }
            if seen.insert(a.clone()) {
                out.push(Target {
                    wallet_id: None,
                    label: "external".into(),
                    address: a,
                });
            }
        }
    }
    if out.is_empty() {
        return Err(AppError::Invalid("no wallets selected".into()));
    }
    Ok(out)
}

/// Collection-wide floor from Seaport `OrderFulfilled` logs inside the window:
/// cheapest per-token native price where `contract`'s tokens change hands.
/// Independent of OpenSea (works on chains/collections OpenSea has no stats for).
async fn seaport_window_floor(
    rpc_url: &str,
    contract: &str,
    from: u64,
    to: u64,
) -> AppResult<Option<U256>> {
    if to < from {
        return Ok(None);
    }
    let mut best: Option<U256> = None;
    for exchange in [SEAPORT_16, SEAPORT_15] {
        let logs = chain::get_logs_adaptive(
            rpc_url,
            from,
            to,
            Some(exchange),
            vec![serde_json::json!(ORDER_FULFILLED_TOPIC)],
            None,
        )
        .await?;
        for lg in &logs {
            let Some(o) = decode_order_fulfilled(lg) else {
                continue;
            };
            if let Some(p) = order_unit_price(&o, contract) {
                if !p.is_zero() && best.map(|b| p < b).unwrap_or(true) {
                    best = Some(p);
                }
            }
        }
    }
    Ok(best)
}

fn parse_hex_u256(s: &str) -> Option<U256> {
    U256::from_str_radix(s.trim_start_matches("0x"), 16).ok()
}

fn gas_cost_of(receipt: &serde_json::Value, tx: &serde_json::Value) -> U256 {
    let gas_used = receipt
        .get("gasUsed")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_u256)
        .unwrap_or(U256::ZERO);
    let price = receipt
        .get("effectiveGasPrice")
        .and_then(|v| v.as_str())
        .and_then(parse_hex_u256)
        .or_else(|| {
            tx.get("gasPrice")
                .and_then(|v| v.as_str())
                .and_then(parse_hex_u256)
        })
        .unwrap_or(U256::ZERO);
    gas_used * price
}

async fn fetch_tx_meta(rpc: &str, tx_hash: &str) -> Option<(String, TxMeta)> {
    let v = chain::rpc_call(rpc, "eth_getTransactionByHash", serde_json::json!([tx_hash]))
        .await
        .ok()?;
    let tx = v.get("result")?.clone();
    if tx.is_null() {
        return None;
    }
    let from = tx.get("from")?.as_str()?.to_lowercase();
    let value = tx
        .get("value")
        .and_then(|x| x.as_str())
        .and_then(parse_hex_u256)
        .unwrap_or(U256::ZERO);
    let receipt = chain::get_receipt(rpc, tx_hash).await.ok().flatten()?;
    let gas_cost = gas_cost_of(&receipt, &tx);
    let mut orders = Vec::new();
    if let Some(logs) = receipt.get("logs").and_then(|l| l.as_array()) {
        for l in logs {
            if let Some(o) = decode_order_fulfilled(l) {
                orders.push(o);
            }
        }
    }
    Some((
        tx_hash.to_string(),
        TxMeta {
            from,
            value,
            gas_cost,
            orders,
        },
    ))
}

async fn opensea_floor(slug: &str) -> Option<U256> {
    let url = format!("https://api.opensea.io/api/v2/collections/{slug}/stats");
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("Aegis/0.1");
    // v2 stats require an API key — use the stored one (API Settings) when the
    // vault is unlocked; without it this 401s and the floor silently falls
    // back to the Seaport window scan.
    if let Some(key) = crate::commands::api_keys::lookup("opensea") {
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&key) {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert("X-API-KEY", value);
            builder = builder.default_headers(headers);
        }
    }
    let client = builder.build().ok()?;
    let v: serde_json::Value = client.get(&url).send().await.ok()?.json().await.ok()?;
    let f = v.pointer("/total/floor_price")?.as_f64()?;
    if !f.is_finite() || f < 0.0 || f > 1_000_000.0 {
        return None;
    }
    Some(U256::from((f * 1e18).round() as u128))
}

/// Collection PnL: 1 contract × N wallets, priced from Seaport OrderFulfilled
/// logs + tx value/gas (no explorer required). Floor: OpenSea stats, else the
/// cheapest in-window sale. Unrealized covers in-window tokens only.
pub async fn scan(
    contract: &str,
    chain_id: i64,
    targets: Vec<Target>,
    window_blocks: u64,
    fee_bps: u64,
    on_progress: &(dyn Fn(&str, u64, u64) + Sync),
) -> AppResult<CollectionPnlResult> {
    let contract = contract.trim().to_lowercase();
    if !is_address(&contract) {
        return Err(AppError::Invalid("contract must be 0x + 40 hex".into()));
    }
    let chains = chain::list_chains()?;
    let ch = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;

    let latest = chain::block_number(&ch.rpc_url).await?;
    let from_block = if window_blocks == 0 {
        0
    } else {
        latest.saturating_sub(window_blocks)
    };

    let mut warnings: Vec<String> = Vec::new();
    let mut target_set: HashSet<String> = HashSet::new();
    for t in &targets {
        target_set.insert(t.address.to_lowercase());
    }
    let or_list = serde_json::Value::Array(
        targets
            .iter()
            .map(|t| serde_json::json!(addr_topic(&t.address)))
            .collect(),
    );

    // ── Phase 1: transfers ──
    // Two queries (in/out) over the whole window via the shared adaptive
    // helper: one big eth_getLogs first, halved on range-limit errors.
    let total_blocks = if latest >= from_block {
        latest - from_block + 1
    } else {
        0
    };
    let prog_total = total_blocks.saturating_mul(2).max(2);
    on_progress("transfers", 0, prog_total);
    let tick_in: &(dyn Fn(u64, u64) + Sync) = &|done: u64, _total: u64| {
        on_progress("transfers", done, prog_total)
    };
    let logs_in = chain::get_logs_adaptive(
        &ch.rpc_url,
        from_block,
        latest,
        Some(&contract),
        vec![
            serde_json::json!(ERC721_TRANSFER_TOPIC),
            serde_json::Value::Null,
            or_list.clone(),
        ],
        Some(tick_in),
    )
    .await?;
    let tick_out: &(dyn Fn(u64, u64) + Sync) = &|done: u64, _total: u64| {
        on_progress("transfers", total_blocks + done, prog_total)
    };
    let logs_out = chain::get_logs_adaptive(
        &ch.rpc_url,
        from_block,
        latest,
        Some(&contract),
        vec![
            serde_json::json!(ERC721_TRANSFER_TOPIC),
            or_list.clone(),
            serde_json::Value::Null,
        ],
        Some(tick_out),
    )
    .await?;
    let mut raw_logs: Vec<serde_json::Value> = Vec::new();
    let mut log_seen: HashSet<(String, u64)> = HashSet::new();
    for l in logs_in.into_iter().chain(logs_out) {
        let txh = l
            .get("transactionHash")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_lowercase();
        let idx = l
            .get("logIndex")
            .and_then(|v| v.as_str())
            .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        if log_seen.insert((txh, idx)) {
            raw_logs.push(l);
        }
    }

    let mut events = parse_erc721_logs(&raw_logs);
    events.retain(|e| e.contract.eq_ignore_ascii_case(&contract));
    events.sort_by(|a, b| {
        a.block_number
            .cmp(&b.block_number)
            .then(a.log_index.cmp(&b.log_index))
    });

    // counts per tx for bundle splitting
    let mut acq: HashMap<String, u64> = HashMap::new();
    let mut outc: HashMap<String, u64> = HashMap::new();
    let mut involved: Vec<String> = Vec::new();
    let mut tx_seen: HashSet<String> = HashSet::new();
    for e in &events {
        if tx_seen.insert(e.tx_hash.clone()) {
            involved.push(e.tx_hash.clone());
        }
        if target_set.contains(&e.to.to_lowercase()) {
            *acq.entry(e.tx_hash.clone()).or_default() += 1;
        }
        if target_set.contains(&e.from.to_lowercase()) {
            *outc.entry(e.tx_hash.clone()).or_default() += 1;
        }
    }

    // ── Phase 2: txs (value + gas + OrderFulfilled from receipt) ──
    let mut txs: HashMap<String, TxMeta> = HashMap::new();
    let mut tx_missing = 0usize;
    let tx_total = involved.len();
    if tx_total > MAX_TXS {
        warnings.push(format!(
            "{tx_total} txs in window — capped at {MAX_TXS}; scope the window smaller for full coverage"
        ));
        involved.truncate(MAX_TXS);
    }
    on_progress("txs", 0, involved.len() as u64);
    let mut set: tokio::task::JoinSet<Option<(String, TxMeta)>> = tokio::task::JoinSet::new();
    let mut i = 0usize;
    let mut done = 0usize;
    while i < involved.len() || !set.is_empty() {
        while i < involved.len() && set.len() < TX_CONCURRENCY {
            let rpc = ch.rpc_url.clone();
            let h = involved[i].clone();
            set.spawn(async move { fetch_tx_meta(&rpc, &h).await });
            i += 1;
        }
        match set.join_next().await {
            Some(Ok(Some((h, m)))) => {
                txs.insert(h, m);
                done += 1;
            }
            Some(_) => {
                tx_missing += 1;
                done += 1;
            }
            None => {}
        }
        on_progress("txs", done as u64, involved.len() as u64);
    }
    if tx_missing > 0 {
        warnings.push(format!(
            "{tx_missing} tx(s) unavailable via RPC — some costs/proceeds/gas may be missing"
        ));
    }

    // ── Phase 3: balances ──
    on_progress("balances", 0, targets.len() as u64);
    let mut balances: HashMap<String, u64> = HashMap::new();
    let mut balance_fail = 0usize;
    for (i, t) in targets.iter().enumerate() {
        match eligibility::erc721_balance_of(&ch.rpc_url, &contract, &t.address).await {
            Ok(b) => {
                balances.insert(t.address.to_lowercase(), b);
            }
            Err(_) => balance_fail += 1,
        }
        on_progress("balances", i as u64 + 1, targets.len() as u64);
    }
    if balance_fail > 0 {
        warnings.push(format!(
            "balanceOf failed for {balance_fail} wallet(s) — window holdings used instead"
        ));
    }

    // ── Accounting per wallet ──
    let mut accs: Vec<(WalletAcc, Option<u64>)> = Vec::new();
    let mut window_sales: HashMap<String, SaleRec> = HashMap::new();
    for t in &targets {
        let lc = t.address.to_lowercase();
        let mine: Vec<OwnershipEvent> = events
            .iter()
            .filter(|e| e.to.to_lowercase() == lc || e.from.to_lowercase() == lc)
            .cloned()
            .collect();
        let acc = account_wallet(&lc, &mine, &txs, &acq, &outc, &contract);
        for s in &acc.sales {
            let entry = window_sales.entry(s.token.clone()).or_insert_with(|| s.clone());
            if (s.block, s.log_index) > (entry.block, entry.log_index) {
                *entry = s.clone();
            }
        }
        accs.push((acc, balances.get(&lc).copied()));
    }

    // ── Phase 4: floor ──
    on_progress("floor", 0, 3);
    let mut floor_wei: Option<U256> = None;
    let mut floor_source: Option<String> = None;
    match OpenSeaClient::new() {
        Ok(client) => {
            match client
                .resolve_collection(&contract, Some(chain_id as u64))
                .await
            {
                Ok(Some(r)) => match opensea_floor(&r.slug).await {
                    Some(f) => {
                        floor_wei = Some(f);
                        floor_source = Some("opensea".into());
                    }
                    None => warnings.push("OpenSea stats unavailable".into()),
                },
                Ok(None) => warnings.push("collection not listed on OpenSea".into()),
                Err(e) => warnings.push(format!("OpenSea resolve failed: {e}")),
            }
        }
        Err(e) => warnings.push(format!("OpenSea client: {e}")),
    }
    on_progress("floor", 1, 3);
    if floor_wei.is_none() {
        match seaport_window_floor(&ch.rpc_url, &contract, from_block, latest).await {
            Ok(Some(p)) => {
                floor_wei = Some(p);
                floor_source = Some("seaport-sales".into());
                warnings.push(
                    "floor from cheapest Seaport sale in window (all-in sale price; fee % applied as the marketplace fee for net unrealized)"
                        .into(),
                );
            }
            Ok(None) => {}
            Err(e) => warnings.push(format!("Seaport floor scan failed: {e}")),
        }
    }
    if floor_wei.is_none() {
        let fallback = window_sales.values().min_by_key(|s| s.price).map(|s| s.price);
        if let Some(p) = fallback {
            floor_wei = Some(p);
            floor_source = Some("window-sales".into());
            warnings.push(
                "floor from cheapest in-window sale (already net of market fee; fee% not applied)"
                    .into(),
            );
        } else {
            warnings.push(
                "no floor price available — unrealized/net unavailable (no fake numbers)".into(),
            );
        }
    }
    on_progress("floor", 3, 3);

    // Marketplace fee is subtracted from EVERY floor that is quoted as an
    // all-in price: OpenSea stats (list price) and Seaport-window sales
    // (what the buyer paid). Both approximate "what you would net if you
    // sold now", matching the seller-net proceeds used for realized. The
    // window-sales fallback is already seller-net, so no fee there.
    let floor_net = floor_wei.map(|f| {
        if matches!(floor_source.as_deref(), Some("opensea") | Some("seaport-sales"))
            && fee_bps < 10_000
        {
            f * U256::from(10_000 - fee_bps) / U256::from(10_000)
        } else {
            f
        }
    });

    // ── Rows + totals ──
    let mut rows = Vec::new();
    let mut totals = TotalsNum::new();
    for (t, (acc, balance)) in targets.iter().zip(accs.iter()) {
        let (row, nums) = build_row(t, acc, *balance, floor_net);
        totals.add(&nums);
        rows.push(row);
    }

    if totals.unpriced > 0 {
        warnings.push(format!(
            "{} sale(s) paid in ERC-20/unknown currency — excluded from realized PnL",
            totals.unpriced
        ));
    }
    if totals.basis_incomplete {
        warnings.push(
            "cost basis incomplete (ERC-20-paid entries and/or holdings outside the window) — see per-wallet flags"
                .into(),
        );
    }

    let finish = totals.finish();
    let result = CollectionPnlResult {
        contract,
        chain_id,
        native_symbol: ch.symbol.clone(),
        window_blocks,
        from_block,
        to_block: latest,
        floor_eth: floor_wei.map(|f| crate::pnl::format_units(f, 18)),
        floor_source,
        fee_bps,
        rows,
        totals: finish,
        warnings,
        scanned_at: crate::db::now_ms(),
    };

    wallet_store::log_activity(
        "pnl.collection",
        &format!(
            "Collection PnL {} chain {chain_id} — {} wallet(s)",
            short_addr(&result.contract),
            result.totals.wallets
        ),
        None,
        true,
    );
    Ok(result)
}

fn short_addr(c: &str) -> String {
    if c.len() > 12 {
        format!("{}…{}", &c[0..7], &c[c.len() - 4..])
    } else {
        c.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_error_detection() {
        assert!(crate::chain::is_range_error(&AppError::Rpc(
            r#"{"code":-32005,"message":"query returned more than 10000 results"}"#.into()
        )));
        assert!(crate::chain::is_range_error(&AppError::Rpc(
            "query exceeds max block range".into()
        )));
        assert!(!crate::chain::is_range_error(&AppError::Rpc("HTTP 429".into())));
        assert!(!crate::chain::is_range_error(&AppError::Rpc(
            "error sending request for url".into()
        )));
    }

    #[test]
    fn seaport_floor_unit_price_from_real_log() {
        // Real OrderFulfilled from Robinhood Chain (tx 0x06009143...4748aa):
        // listing of CryptoP0ns #2254, all-in price 0.00046 ETH.
        let lg: serde_json::Value = serde_json::json!({
            "topics": [
                "0x9d9af8e38d66c62e2c12f0225249fd9d721c54b83f48d9352c97c6cacdcb6f31",
                "0x0000000000000000000000000581182c1c3e918371d83729c8874ac7597c7300",
                "0x000000000000000000000000000056f7000000ece9003ca63978907a00ffd100"
            ],
            "data": "0xc752ec775bc360cd8fd0541816cdf427530e803914a6bab3e0da5d0695e89aac0000000000000000000000007b3eea61b543b8afa72cb25975d87a06265310b10000000000000000000000000000000000000000000000000000000000000080000000000000000000000000000000000000000000000000000000000000012000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000e1dd28bb9c61dac72f7a47f0a2c712eb4976ec2f00000000000000000000000000000000000000000000000000000000000008ce0000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000300000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000018943f866a0000000000000000000000000000581182c1c3e918371d83729c8874ac7597c73000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000042f055db0000000000000000000000000000000a26b00c1f0df003000390027140000faa719000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000014eb1ad470000000000000000000000000007b3eea61b543b8afa72cb25975d87a06265310b1"
        });
        let o = decode_order_fulfilled(&lg).expect("fixture decodes");
        let p = order_unit_price(&o, "0xe1dd28bb9c61dac72f7a47f0a2c712eb4976ec2f")
            .expect("unit price for contract");
        assert_eq!(p, U256::from(460_000_000_000_000u64));
        assert!(
            order_unit_price(&o, "0x0000000000000000000000000000000000000001").is_none()
        );
    }

    const FIXTURE_TOPICS: [&str; 3] = [
        "0x9d9af8e38d66c62e2c12f0225249fd9d721c54b83f48d9352c97c6cacdcb6f31",
        "0x00000000000000000000000001783534a636ca6525acf48e481d5c7f3584a29c",
        "0x0000000000000000000000000000000000000000000000000000000000000000",
    ];
    const FIXTURE_DATA: &str = "0xbe8fa2fd9c7beb0d7ebe79e24f43c83f71705cc25a7d012efbc0958d6770664000000000000000000000000018ec1b1286502e24a0d5f9f588c3bb169959cb86000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000001000000000000000000000000000000000000000000000000000000000000000200000000000000000000000097630aa70ab14ed9883b41dafccbc11349723043000000000000000000000000000000000000000000000000000000000001c2e100000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000005abe504cda600000000000000000000000000001783534a636ca6525acf48e481d5c7f3584a29c0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000eaa64e5a20000000000000000000000000000000a26b00c1f0df003000390027140000faa719";

    #[test]
    fn topic0_matches_canonical_signature() {
        let hash = alloy::primitives::keccak256(ORDER_FULFILLED_SIG.as_bytes());
        assert_eq!(format!("0x{}", hex::encode(hash)), ORDER_FULFILLED_TOPIC);
    }

    fn fixture_log() -> serde_json::Value {
        serde_json::json!({
            "topics": FIXTURE_TOPICS,
            "data": FIXTURE_DATA,
        })
    }

    #[test]
    fn decode_fixture_order_fulfilled() {
        let o = decode_order_fulfilled(&fixture_log()).expect("head-4 decode");
        assert_eq!(o.offer.len(), 1);
        assert_eq!(o.offer[0].item_type, 2);
        assert!(
            o.offer[0].token.eq_ignore_ascii_case("0x97630aa70ab14ed9883b41dafccbc11349723043")
        );
        assert_eq!(o.offer[0].identifier, U256::from(115425u64));
        assert_eq!(o.offer[0].amount, U256::from(1u64));
        assert_eq!(o.consideration.len(), 2);
        assert_eq!(o.consideration[0].item_type, 0);
        assert_eq!(
            o.consideration[0].amount,
            U256::from(25_542_000_000_000_000u64)
        );
        assert_eq!(
            o.consideration[0].recipient.as_deref(),
            Some("0x01783534a636ca6525acf48e481d5c7f3584a29c")
        );
        assert_eq!(
            o.consideration[1].amount,
            U256::from(258_000_000_000_000u64)
        );
        assert_eq!(
            o.consideration[1].recipient.as_deref(),
            Some("0x0000a26b00c1f0df003000390027140000faa719")
        );
        // full buyer cost = proceeds + fee
        assert_eq!(o.native_total, U256::from(25_800_000_000_000_000u64));
    }

    #[test]
    fn decoder_rejects_wrong_topic_and_short_data() {
        let mut bad = fixture_log();
        bad["topics"][0] = serde_json::json!(ERC721_TRANSFER_TOPIC);
        assert!(decode_order_fulfilled(&bad).is_none());
        let mut short = fixture_log();
        short["data"] = serde_json::json!("0x1234");
        assert!(decode_order_fulfilled(&short).is_none());
    }

    fn ev(from: &str, to: &str, token: &str, tx: &str, block: u64, idx: u64) -> OwnershipEvent {
        OwnershipEvent {
            contract: CONTRACT.into(),
            token_id: token.into(),
            from: from.into(),
            to: to.into(),
            block_number: block,
            log_index: idx,
            tx_hash: tx.into(),
        }
    }

    const CONTRACT: &str = "0xe1dd28bb9c61dac72f7a47f0a2c712eb4976ec2f";
    const W: &str = "0x1111111111111111111111111111111111111111";
    const SELLER: &str = "0x2222222222222222222222222222222222222222";

    fn eth(n: u64) -> U256 {
        U256::from(n) * U256::from(10u64.pow(18))
    }

    fn order(native_to_seller: Option<U256>, nft_token: &str) -> FulfilledOrder {
        let consideration = match native_to_seller {
            Some(v) => vec![OrderItem {
                item_type: 0,
                token: ZERO_ADDR.into(),
                identifier: U256::ZERO,
                amount: v,
                recipient: Some(SELLER.into()),
            }],
            None => vec![OrderItem {
                item_type: 1,
                token: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".into(),
                identifier: U256::ZERO,
                amount: eth(1),
                recipient: Some(W.into()),
            }],
        };
        let native_total = consideration
            .iter()
            .filter(|i| i.item_type == 0)
            .fold(U256::ZERO, |a, b| a.saturating_add(b.amount));
        FulfilledOrder {
            offer: vec![OrderItem {
                item_type: 2,
                token: CONTRACT.into(),
                identifier: U256::from_str_radix(nft_token, 10).unwrap(),
                amount: U256::from(1u64),
                recipient: None,
            }],
            consideration,
            native_total,
        }
    }

    fn setup_txs() -> HashMap<String, TxMeta> {
        let mut txs = HashMap::new();
        // T1: free mint by wallet (gas 0.001)
        txs.insert(
            "0xt1".into(),
            TxMeta {
                from: W.into(),
                value: U256::ZERO,
                gas_cost: U256::from(10u64.pow(15)),
                orders: vec![],
            },
        );
        // T2: buy token 2 for 0.1 ETH (Seaport listing fulfilment)
        txs.insert(
            "0xt2".into(),
            TxMeta {
                from: W.into(),
                value: eth(1) / U256::from(10), // 0.1 — order cost takes precedence anyway
                gas_cost: U256::from(2u64 * 10u64.pow(15)),
                orders: vec![order(Some(eth(1) / U256::from(10)), "2")],
            },
        );
        // T3: sell token 1 for 0.2 ETH to wallet
        let mut sell = order(Some(eth(1) / U256::from(5)), "1");
        sell.consideration[0].recipient = Some(W.into());
        sell.consideration[0].amount = eth(1) / U256::from(5);
        txs.insert(
            "0xt3".into(),
            TxMeta {
                from: W.into(),
                value: U256::ZERO,
                gas_cost: U256::from(500_000_000_000_000u64), // 0.0005
                orders: vec![sell],
            },
        );
        txs
    }

    #[test]
    fn accounting_mint_buy_sell() {
        let txs = setup_txs();
        let events = vec![
            ev(ZERO_ADDR, W, "1", "0xt1", 10, 0),
            ev(SELLER, W, "2", "0xt2", 20, 0),
            ev(W, SELLER, "1", "0xt3", 30, 0),
        ];
        let mut acq = HashMap::new();
        acq.insert("0xt1".to_string(), 1u64);
        acq.insert("0xt2".to_string(), 1u64);
        let mut outc = HashMap::new();
        outc.insert("0xt3".to_string(), 1u64);

        let acc = account_wallet(W, &events, &txs, &acq, &outc, CONTRACT);
        assert_eq!(acc.minted, 1);
        assert_eq!(acc.bought, 1);
        assert_eq!(acc.sold, 1);
        assert_eq!(acc.spent, eth(1) / U256::from(10));
        assert_eq!(acc.gas, U256::from(3500_000_000_000_000u64));
        assert_eq!(acc.realized, {
            let p = eth(1) / U256::from(5);
            (u256_to_u128(p) as i128) - 0
        });
        assert!(acc.held.contains("2"));
        assert!(!acc.held.contains("1"));
        assert!(!acc.basis_unknown);
        assert_eq!(acc.unpriced_sales, 0);

        // row with floor 0.05, no fee: unrealized = 0.05 - 0.1 = -0.05
        let floor_net = eth(1) / U256::from(20);
        let t = Target {
            wallet_id: Some(1),
            label: "w1".into(),
            address: W.into(),
        };
        let (row, nums) = build_row(&t, &acc, Some(1), Some(floor_net));
        assert_eq!(row.minted, 1);
        assert_eq!(row.bought, 1);
        assert_eq!(row.sold, 1);
        assert_eq!(row.balance, 1);
        assert!(!row.basis_incomplete);
        // net = realized 0.2 + unrealized -0.05 - gas 0.0035 = 0.1465
        assert_eq!(row.net_eth.as_deref(), Some("+0.1465"));
        assert_eq!(row.unrealized_eth.as_deref(), Some("-0.05"));
        let expected_roi = 0.1465 / 0.1035 * 100.0;
        let roi = row.roi_pct.expect("roi");
        assert!((roi - expected_roi).abs() < 1e-9);
        assert!(nums.net.is_some());
    }

    #[test]
    fn free_mint_and_gift_and_unpriced_sale() {
        let mut txs = HashMap::new();
        txs.insert(
            "0xt1".into(),
            TxMeta {
                from: W.into(),
                value: U256::ZERO,
                gas_cost: U256::ZERO,
                orders: vec![],
            },
        );
        // gift: someone else sent it, no order, no value
        txs.insert(
            "0xt2".into(),
            TxMeta {
                from: SELLER.into(),
                value: U256::ZERO,
                gas_cost: U256::ZERO,
                orders: vec![],
            },
        );
        // unpriced sale: order covers token but no native to wallet (ERC-20 bid)
        txs.insert(
            "0xt3".into(),
            TxMeta {
                from: W.into(),
                value: U256::ZERO,
                gas_cost: U256::ZERO,
                orders: vec![order(None, "2")],
            },
        );
        let events = vec![
            ev(ZERO_ADDR, W, "1", "0xt1", 10, 0),
            ev(SELLER, W, "2", "0xt2", 20, 0),
            ev(W, SELLER, "2", "0xt3", 30, 0),
        ];
        let mut acq = HashMap::new();
        acq.insert("0xt1".to_string(), 1);
        acq.insert("0xt2".to_string(), 1);
        let mut outc = HashMap::new();
        outc.insert("0xt3".to_string(), 1);

        let acc = account_wallet(W, &events, &txs, &acq, &outc, CONTRACT);
        assert_eq!(acc.minted, 1);
        assert_eq!(acc.transfer_in, 1);
        assert_eq!(acc.unpriced_sales, 1);
        assert_eq!(acc.sold, 0);
        assert_eq!(acc.spent, U256::ZERO);
        assert_eq!(acc.realized, 0);
        assert!(acc.held.contains("1"));
        assert!(!acc.held.contains("2"));

        // floor missing → unrealized/net unavailable (honest None)
        let t = Target {
            wallet_id: None,
            label: "w".into(),
            address: W.into(),
        };
        let (row, _) = build_row(&t, &acc, Some(1), None);
        assert!(row.unrealized_eth.is_none());
        assert!(row.net_eth.is_none());
        assert!(row.roi_pct.is_none());
    }

    #[test]
    fn prewindow_holdings_flagged_excluded_from_unrealized() {
        let txs = setup_txs();
        let events = vec![
            ev(ZERO_ADDR, W, "1", "0xt1", 10, 0),
            ev(SELLER, W, "2", "0xt2", 20, 0),
            ev(W, SELLER, "1", "0xt3", 30, 0),
        ];
        let acq: HashMap<String, u64> = HashMap::new();
        let outc: HashMap<String, u64> = HashMap::new();
        let acc = account_wallet(W, &events, &txs, &acq, &outc, CONTRACT);
        // balance says 3 but window only covers 1 held token → 2 out-of-window
        let t = Target {
            wallet_id: Some(1),
            label: "w1".into(),
            address: W.into(),
        };
        let (row, _) = build_row(&t, &acc, Some(3), Some(eth(1)));
        assert_eq!(row.out_of_window, 2);
        assert!(row.basis_incomplete);
        // unrealized = floor*held(1) - basis(0.1) = 0.9
        assert_eq!(row.unrealized_eth.as_deref(), Some("+0.9"));
    }

    #[test]
    fn fmt_signed_formats_units() {
        assert_eq!(fmt_signed(0), "0");
        assert_eq!(fmt_signed(1_500_000_000_000_000_000), "+1.5");
        assert_eq!(fmt_signed(-250_000_000_000_000_000), "-0.25");
    }

    #[test]
    fn head3_layout_decodes() {
        // Build a head-3 payload: word0 orderHash, word1 = 0x60, word2 = 0x100.
        let mut words: Vec<String> = Vec::new();
        words.push(format!("{:064x}", 1u64)); // orderHash
        words.push(format!("{:064x}", 0x60u64)); // offer off
        words.push(format!("{:064x}", 0x100u64)); // consideration off
        words.push(format!("{:064x}", 1u64)); // offer len @0x60
        words.push(format!("{:064x}", 2u64)); // item type
        words.push(format!("{:064x}", 0u64)); // token (zero)
        words.push(format!("{:064x}", 7u64)); // identifier
        words.push(format!("{:064x}", 1u64)); // amount
        words.push(format!("{:064x}", 1u64)); // consideration len @0x100
        words.push(format!("{:064x}", 0u64)); // type native
        words.push(format!("{:064x}", 0u64)); // token
        words.push(format!("{:064x}", 0u64)); // id
        words.push(format!("{:064x}", 9u64)); // amount
        words.push(format!("{:064x}", 0x3333u64)); // recipient (single word, right-aligned)
        let log = serde_json::json!({
            "topics": FIXTURE_TOPICS,
            "data": format!("0x{}", words.join("")),
        });
        let o = decode_order_fulfilled(&log).expect("head-3 decode");
        assert_eq!(o.offer[0].identifier, U256::from(7u64));
        assert_eq!(o.consideration[0].amount, U256::from(9u64));
        assert_eq!(
            o.consideration[0].recipient.as_deref(),
            Some("0x0000000000000000000000000000000000003333")
        );
        assert_eq!(o.native_total, U256::from(9u64));
    }

    #[test]
    fn bundle_split_divides_cost() {
        // two NFTs received in one tx → cost split by acq count
        let mut txs = HashMap::new();
        txs.insert(
            "0xt1".into(),
            TxMeta {
                from: W.into(),
                value: eth(1), // 1 ETH for a 2-NFT bundle, no order
                gas_cost: U256::ZERO,
                orders: vec![],
            },
        );
        let events = vec![
            ev(SELLER, W, "5", "0xt1", 10, 0),
            ev(SELLER, W, "6", "0xt1", 10, 1),
        ];
        let mut acq = HashMap::new();
        acq.insert("0xt1".to_string(), 2);
        let outc: HashMap<String, u64> = HashMap::new();
        let acc = account_wallet(W, &events, &txs, &acq, &outc, CONTRACT);
        assert_eq!(acc.bought, 2);
        assert_eq!(acc.spent, eth(1)); // 0.5 + 0.5
    }
}
