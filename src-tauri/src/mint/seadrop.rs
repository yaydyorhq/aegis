use crate::chain;
use crate::error::{AppError, AppResult};
use serde::Serialize;

/// OpenSea SeaDrop singleton — public mint entrypoint (not the NFT proxy).
pub const SEADROP_ADDRESS: &str = "0x00005EA00Ac477B1030CE78506496e8C2dE24bf5";
/// OpenSea default fee collector when the drop does not restrict recipients.
pub const OPENSEA_FEE_RECIPIENT: &str = "0x0000a26b00c1F0DF003000390027140000fAa719";

// getPublicDrop(address) / getAllowedFeeRecipients(address)
const SEL_GET_PUBLIC_DROP: &str = "0xbc6a629c";
const SEL_GET_ALLOWED_FEE: &str = "0x68632274";
// totalSupply() / maxSupply() — must be read on the proxy: implementation
// storage is empty, so a minimal-proxy clone reports 0 there.
const SEL_TOTAL_SUPPLY: &str = "0x18160ddd";
const SEL_MAX_SUPPLY: &str = "0xd5abeb01";

#[derive(Serialize, Clone)]
pub struct PublicDrop {
    pub mint_price_wei: String,
    pub start_time: i64,
    pub end_time: i64,
    pub max_per_wallet: i64,
    pub fee_bps: i64,
    pub restrict_fee_recipients: bool,
}

#[derive(Serialize, Clone)]
pub struct SeaDropPlan {
    /// Always the SeaDrop singleton — this is the tx `to`.
    pub to: String,
    /// NFT collection contract (first mintPublic arg).
    pub nft_contract: String,
    pub fee_recipient: String,
    pub fee_source: String,
    pub calldata: String,
    /// mint_price × quantity
    pub value_wei: String,
    pub value_eth: String,
    pub quantity: i64,
    pub drop: PublicDrop,
    pub live: bool,
    /// Minted count read from the proxy, when readable.
    pub total_supply: Option<u64>,
    /// Collection cap when one is enforced; `None` when unset (unlimited).
    pub max_supply: Option<u64>,
    /// `max_supply - total_supply` when both are known.
    pub remaining: Option<u64>,
}

fn word_addr(addr: &str) -> AppResult<String> {
    let a = alloy::primitives::Address::parse_checksummed(addr, None)
        .or_else(|_| addr.parse::<alloy::primitives::Address>())
        .map_err(|_| AppError::Invalid(format!("bad address: {addr}")))?;
    Ok(hex::encode(a.as_slice()))
}

fn word_u256(v: alloy::primitives::U256) -> String {
    format!("{:064x}", v)
}

fn pad_addr_arg(addr: &str) -> AppResult<String> {
    Ok(format!("{:0>64}", word_addr(addr)?))
}

/// Decode a 32-byte-aligned eth_call result into U256 words.
fn decode_words(data_hex: &str) -> AppResult<Vec<alloy::primitives::U256>> {
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

/// Read a uint64-returning view on `to`. `None` on any decode/transport error
/// so supply discovery never blocks a plan the caller can still use.
async fn read_u64(rpc: &str, to: &str, selector: &str) -> Option<u64> {
    let ret = eth_call(rpc, to, selector).await.ok()?;
    let w = decode_words(&ret).ok()?;
    u64::try_from(w.first().copied()?).ok()
}

/// Remaining supply when the collection enforces a cap. An unset cap decodes to
/// `max == 0`, which means unlimited — report `None`, never "zero left".
/// Exhausted clamps to `Some(0)` (not `None`) so the caller's fast-fail fires.
pub fn remaining_supply(total: Option<u64>, max: Option<u64>) -> Option<u64> {
    let m = max?;
    if m == 0 {
        return None;
    }
    Some(m.saturating_sub(total.unwrap_or(0)))
}

/// Read the public drop config for `nft_contract` from the SeaDrop singleton.
pub async fn fetch_public_drop(rpc: &str, nft_contract: &str) -> AppResult<PublicDrop> {
    let data = format!("{SEL_GET_PUBLIC_DROP}{}", pad_addr_arg(nft_contract)?);
    let ret = eth_call(rpc, SEADROP_ADDRESS, &data).await?;
    let w = decode_words(&ret)?;
    if w.len() < 6 {
        return Err(AppError::Rpc(format!(
            "getPublicDrop returned {} words (need 6)",
            w.len()
        )));
    }
    let start = u64::try_from(w[1]).unwrap_or(0) as i64;
    let end = u64::try_from(w[2]).unwrap_or(0) as i64;
    let max_pw = u64::try_from(w[3]).unwrap_or(0) as i64;
    // Unset mapping entry decodes to all zeros.
    if start == 0 && end == 0 && max_pw == 0 {
        return Err(AppError::NotFound(
            "no SeaDrop public drop for this contract".into(),
        ));
    }
    Ok(PublicDrop {
        mint_price_wei: w[0].to_string(),
        start_time: start,
        end_time: end,
        max_per_wallet: max_pw,
        fee_bps: u64::try_from(w[4]).unwrap_or(0) as i64,
        restrict_fee_recipients: !w[5].is_zero(),
    })
}

async fn allowed_fee_recipients(rpc: &str, nft_contract: &str) -> AppResult<Vec<String>> {
    let data = format!("{SEL_GET_ALLOWED_FEE}{}", pad_addr_arg(nft_contract)?);
    let ret = eth_call(rpc, SEADROP_ADDRESS, &data).await?;
    let raw = ret.trim_start_matches("0x");
    if raw.len() < 64 {
        return Ok(vec![]);
    }
    // ABI: offset (32) | length (3) | items...
    let words = decode_words(&ret)?;
    if words.is_empty() {
        return Ok(vec![]);
    }
    let offset = usize::try_from(u64::try_from(words[0]).unwrap_or(0)).unwrap_or(0);
    // offset is in bytes from start of ret after selector — words are 32-byte slots
    let slot = offset / 32;
    if slot + 1 >= words.len() {
        return Ok(vec![]);
    }
    let len = usize::try_from(u64::try_from(words[slot]).unwrap_or(0)).unwrap_or(0);
    let mut out = Vec::new();
    for i in 0..len {
        let idx = slot + 1 + i;
        if idx >= words.len() {
            break;
        }
        let w = format!("{:064x}", words[idx]);
        out.push(format!("0x{}", &w[24..]));
    }
    Ok(out)
}

/// Encode mintPublic(nftContract, feeRecipient, address(0), quantity).
/// minterIfNotPayer = 0 means credit the caller — same calldata for every wallet.
pub fn encode_mint_public(nft_contract: &str, fee_recipient: &str, quantity: i64) -> AppResult<String> {
    let qty = alloy::primitives::U256::from(quantity.max(1) as u64);
    Ok(format!(
        "0x161ac21f{}{}{}{}",
        pad_addr_arg(nft_contract)?,
        pad_addr_arg(fee_recipient)?,
        pad_addr_arg("0x0000000000000000000000000000000000000000")?,
        word_u256(qty),
    ))
}

/// Build a local public-mint plan from on-chain SeaDrop state (no OpenSea API).
pub async fn build_public_mint_plan(
    rpc: &str,
    nft_contract: &str,
    quantity: i64,
) -> AppResult<SeaDropPlan> {
    if quantity < 1 {
        return Err(AppError::Invalid("quantity >= 1".into()));
    }
    let nft = nft_contract.trim();
    if nft.len() != 42 || !nft.starts_with("0x") {
        return Err(AppError::Invalid(
            "nft contract must be 0x + 40 hex".into(),
        ));
    }

    let drop = fetch_public_drop(rpc, nft).await?;

    let allowed = allowed_fee_recipients(rpc, nft).await.unwrap_or_default();
    let (fee_recipient, fee_source) = if let Some(first) = allowed.into_iter().next() {
        (first, "allowed fee recipient on-chain".to_string())
    } else if drop.restrict_fee_recipients {
        return Err(AppError::Invalid(
            "drop restricts fee recipients but none are allowed — cannot build public mint"
                .into(),
        ));
    } else {
        (
            OPENSEA_FEE_RECIPIENT.to_string(),
            "OpenSea default (drop does not restrict)".to_string(),
        )
    };

    let price = drop
        .mint_price_wei
        .parse::<alloy::primitives::U256>()
        .map_err(|_| AppError::Invalid("bad mint price".into()))?;
    let value = price * alloy::primitives::U256::from(quantity as u64);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let live = now >= drop.start_time && now < drop.end_time;

    // Preflight: minted/cap live in the proxy's storage, never the impl's.
    let total_supply = read_u64(rpc, nft, SEL_TOTAL_SUPPLY).await;
    let max_supply = read_u64(rpc, nft, SEL_MAX_SUPPLY).await.filter(|m| *m > 0);
    let remaining = remaining_supply(total_supply, max_supply);
    if let Some(left) = remaining {
        if (left as i64) < quantity {
            return Err(AppError::Invalid(format!(
                "supply exhausted: {left} left of {} minted — quantity {quantity} would revert",
                total_supply.unwrap_or(0)
            )));
        }
    }

    let calldata = encode_mint_public(nft, &fee_recipient, quantity)?;
    Ok(SeaDropPlan {
        to: SEADROP_ADDRESS.to_string(),
        nft_contract: nft.to_string(),
        fee_recipient,
        fee_source,
        calldata,
        value_wei: value.to_string(),
        value_eth: format_eth(value),
        quantity,
        drop,
        live,
        total_supply,
        max_supply,
        remaining,
    })
}

pub fn format_eth(wei: alloy::primitives::U256) -> String {
    let s = wei.to_string();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_mint_public_shape() {
        let nft = "0x1ba3181d446ef973b7819add816ebc08eaa3b397";
        let fee = OPENSEA_FEE_RECIPIENT;
        let cd = encode_mint_public(nft, fee, 1).unwrap();
        assert!(cd.starts_with("0x161ac21f"));
        // selector 8 hex + 4 * 64
        assert_eq!(cd.len(), 2 + 8 + 4 * 64);
        // minterIfNotPayer zero word at the end before qty… actually 3rd word
        let body = &cd[10..];
        let w2 = &body[128..192];
        assert_eq!(w2, &"0".repeat(64));
        let w3 = &body[192..256];
        assert!(w3.ends_with('1'));
    }

    #[test]
    fn format_eth_small() {
        let v = alloy::primitives::U256::from(5_000_000_000_000_000u64);
        assert_eq!(format_eth(v), "0.005");
    }

    #[test]
    fn remaining_supply_respects_unset_and_exhausted_caps() {
        assert_eq!(remaining_supply(Some(10), Some(3333)), Some(3323));
        // max == 0 decodes from an unset cap: unlimited, not "zero left".
        assert_eq!(remaining_supply(Some(10), Some(0)), None);
        assert_eq!(remaining_supply(Some(10), None), None);
        assert_eq!(remaining_supply(None, Some(100)), Some(100));
        // Exhausted stays non-negative so the fast-fail fires instead of wrapping.
        assert_eq!(remaining_supply(Some(100), Some(50)), Some(0));
    }

    #[test]
    fn selectors_match_seadrop_abi() {
        let cases = [
            ("getPublicDrop(address)", SEL_GET_PUBLIC_DROP),
            ("getAllowedFeeRecipients(address)", SEL_GET_ALLOWED_FEE),
            (
                "mintPublic(address,address,address,uint256)",
                "0x161ac21f",
            ),
        ];
        for (sig, expected) in cases {
            let h = alloy::primitives::keccak256(sig.as_bytes());
            let got = format!("0x{}", &hex::encode(&h[..4]));
            assert_eq!(got, expected, "selector mismatch for {sig}");
        }
    }

    #[test]
    fn decodes_allowed_fee_recipients_offset() {
        // ABI: offset(0x20) | len(2) | addr1 | addr2
        let a1 = "0000000000000000000000001111111111111111111111111111111111111111";
        let a2 = "0000000000000000000000002222222222222222222222222222222222222222";
        let ret = format!(
            "0x{}{}{}{}",
            "0000000000000000000000000000000000000000000000000000000000000020",
            "0000000000000000000000000000000000000000000000000000000000000002",
            a1,
            a2
        );
        // Reuse the same decode path allowed_fee_recipients uses via words.
        let words = decode_words(&ret).unwrap();
        assert_eq!(words.len(), 4);
        let offset = usize::try_from(u64::try_from(words[0]).unwrap_or(0)).unwrap_or(0);
        let slot = offset / 32;
        let len = usize::try_from(u64::try_from(words[slot]).unwrap_or(0)).unwrap_or(0);
        assert_eq!(len, 2);
        let w1 = format!("{:064x}", words[slot + 1]);
        assert_eq!(&w1[24..], &a1[24..]);
    }
}
