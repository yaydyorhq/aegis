use crate::chain;
use crate::error::{AppError, AppResult};
use crate::mint::seadrop::{self, PublicDrop};

/// balanceOf(address)
const SEL_BALANCE_OF: &str = "0x70a08231";
/// ERC721SeaDrop.getMintStats(address minter)
///   → (minterNumMinted, currentTotalSupply, maxSupply).
/// Lives on the NFT token, not the SeaDrop singleton — upstream ISeaDrop has
/// no getMintStats at all, and neither old constant ("getMintStats(address,
/// address)" 0xec1c35be nor "mintStatsForMinter(address)" 0x25e31cd4)
/// resolves on 4byte.directory.
const SEL_GET_MINT_STATS: &str = "0x840e15d4";

fn pad_word_addr(addr: &str) -> AppResult<String> {
    let a = alloy::primitives::Address::parse_checksummed(addr, None)
        .or_else(|_| addr.parse::<alloy::primitives::Address>())
        .map_err(|_| AppError::Invalid(format!("bad address: {addr}")))?;
    Ok(format!("{:0>64}", hex::encode(a.as_slice())))
}

async fn eth_call(rpc: &str, to: &str, data: &str) -> AppResult<String> {
    let out = chain::rpc_call(
        rpc,
        "eth_call",
        serde_json::json!([{ "to": to, "data": data }, "latest"]),
    )
    .await?;
    out.get("result")
        .and_then(|r| r.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| AppError::Rpc("eth_call missing result".into()))
}

fn decode_u256_words(data_hex: &str) -> AppResult<Vec<alloy::primitives::U256>> {
    let raw = data_hex.trim_start_matches("0x");
    if raw.is_empty() || !raw.len().is_multiple_of(64) {
        return Err(AppError::Rpc(format!(
            "unexpected eth_call length {}",
            raw.len()
        )));
    }
    Ok(raw
        .as_bytes()
        .chunks(64)
        .map(|c| {
            let s = std::str::from_utf8(c).unwrap_or("");
            alloy::primitives::U256::from_str_radix(s, 16).unwrap_or_default()
        })
        .collect())
}

/// ERC-721 balanceOf — O(1); replaces multi-chunk eth_getLogs scans.
pub async fn erc721_balance_of(rpc: &str, collection: &str, owner: &str) -> AppResult<u64> {
    let data = format!("{SEL_BALANCE_OF}{}", pad_word_addr(owner)?);
    let ret = eth_call(rpc, collection, &data).await?;
    let w = decode_u256_words(&ret)?;
    let v = w.first().copied().unwrap_or_default();
    Ok(u64::try_from(v).unwrap_or(u64::MAX))
}

/// Minted count for the wallet on a SeaDrop drop (0 when unknown).
/// Reads the NFT token's own `getMintStats(address)` — word 0 is
/// `minterNumMinted` (word 1 is the collection total supply, NOT per-wallet).
pub async fn minted_quantity(rpc: &str, collection: &str, minter: &str) -> Option<u64> {
    let m = pad_word_addr(minter).ok()?;
    let ret = eth_call(rpc, collection, &format!("{SEL_GET_MINT_STATS}{m}"))
        .await
        .ok()?;
    let w = decode_u256_words(&ret).ok()?;
    w.first().and_then(|v| u64::try_from(*v).ok())
}

/// Public-drop window: live iff start ≤ now, and now < end when end is set.
/// `end_time == 0` means open-ended (SeaDrop unset only when start/end/max are all 0).
pub fn public_drop_live(drop: &PublicDrop, now_secs: i64) -> bool {
    if now_secs < drop.start_time {
        return false;
    }
    drop.end_time == 0 || now_secs < drop.end_time
}

pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn format_ts(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| ts.to_string())
}

fn format_wei_eth(wei_str: &str) -> String {
    match wei_str.parse::<alloy::primitives::U256>() {
        Ok(v) => {
            let s = v.to_string();
            if s == "0" {
                return "0".into();
            }
            let out = if s.len() <= 18 {
                let pad = format!("{}{}", "0".repeat(18 - s.len()), s);
                format!("0.{pad}")
            } else {
                let (h, f) = s.split_at(s.len() - 18);
                format!("{h}.{f}")
            };
            let out = out.trim_end_matches('0');
            out.strip_suffix('.').map(|x| x.to_string()).unwrap_or_else(|| out.to_string())
        }
        Err(_) => wei_str.to_string(),
    }
}

/// Load the public drop once per collection. `None` = not a public SeaDrop drop
/// (missing entry or non-SeaDrop contract) — caller falls back to holdings.
pub async fn fetch_drop_or_none(rpc: &str, collection: &str) -> Option<PublicDrop> {
    seadrop::fetch_public_drop(rpc, collection).await.ok()
}

/// Eligibility for one wallet.
/// - Public drop configured → stage rules (window, max/wallet, balance).
/// - Else → ERC-721 balanceOf (holder-gated / already owns).
pub async fn check_eligibility(
    rpc: &str,
    wallet: &str,
    collection: &str,
    drop: Option<&PublicDrop>,
) -> AppResult<(bool, String)> {
    let col = collection.trim();
    if col.len() != 42 || !col.starts_with("0x") {
        return Err(AppError::Invalid(
            "collection must be 0x + 40 hex".into(),
        ));
    }

    if let Some(drop) = drop {
        return check_public_drop(rpc, wallet, col, drop).await;
    }

    let held = erc721_balance_of(rpc, col, wallet).await?;
    if held > 0 {
        Ok((true, format!("{held} token(s) held")))
    } else {
        Ok((
            false,
            "no SeaDrop public drop; wallet holds 0 tokens".into(),
        ))
    }
}

async fn check_public_drop(
    rpc: &str,
    wallet: &str,
    collection: &str,
    drop: &PublicDrop,
) -> AppResult<(bool, String)> {
    let now = now_secs();
    let price = format_wei_eth(&drop.mint_price_wei);
    let max = drop.max_per_wallet;

    if now < drop.start_time {
        return Ok((
            false,
            format!(
                "public drop opens {} (price {price} ETH, max {max}/wallet)",
                format_ts(drop.start_time)
            ),
        ));
    }
    if !public_drop_live(drop, now) {
        return Ok((
            false,
            format!(
                "public drop ended {} (price {price} ETH)",
                format_ts(drop.end_time)
            ),
        ));
    }

    let minted = minted_quantity(rpc, collection, wallet).await.unwrap_or(0);
    let held = erc721_balance_of(rpc, collection, wallet).await.unwrap_or(0);
    let used = minted.max(held);

    if max > 0 && used >= max as u64 {
        return Ok((
            false,
            format!("public drop live but max reached ({used}/{max})"),
        ));
    }

    let price_wei = drop
        .mint_price_wei
        .parse::<alloy::primitives::U256>()
        .unwrap_or_default();
    if !price_wei.is_zero() {
        let bal_hex = chain::native_balance(rpc, wallet).await?;
        let bal = alloy::primitives::U256::from_str_radix(
            bal_hex.trim_start_matches("0x"),
            16,
        )
        .unwrap_or_default();
        if bal < price_wei {
            return Ok((
                false,
                format!(
                    "public drop live but balance short (need {price} ETH, have {} ETH)",
                    format_wei_eth(&bal.to_string())
                ),
            ));
        }
    }

    Ok((
        true,
        format!(
            "public drop live (price {price} ETH, max {max}/wallet, minted {used})"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drop(start: i64, end: i64, max: i64) -> PublicDrop {
        PublicDrop {
            mint_price_wei: "380000000000000".into(),
            start_time: start,
            end_time: end,
            max_per_wallet: max,
            fee_bps: 0,
            restrict_fee_recipients: false,
        }
    }

    #[test]
    fn live_window() {
        let d = drop(100, 200, 1);
        assert!(!public_drop_live(&d, 99));
        assert!(public_drop_live(&d, 100));
        assert!(public_drop_live(&d, 199));
        assert!(!public_drop_live(&d, 200));
    }

    #[test]
    fn open_ended_window() {
        let d = drop(100, 0, 5);
        assert!(public_drop_live(&d, 1000));
    }

    #[test]
    fn format_wei() {
        assert_eq!(format_wei_eth("380000000000000"), "0.00038");
        assert_eq!(format_wei_eth("1000000000000000000"), "1");
    }

    /// The minted-count read must target ERC721SeaDrop's real
    /// `getMintStats(address)` — a wrong selector silently disables the
    /// per-wallet cap check (minted always 0).
    #[test]
    fn get_mint_stats_selector_matches_signature() {
        let h = alloy::primitives::keccak256(b"getMintStats(address)");
        assert_eq!(
            format!("0x{}", hex::encode(&h[..4])),
            SEL_GET_MINT_STATS
        );
    }
}
