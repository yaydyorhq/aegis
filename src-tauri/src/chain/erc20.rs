use crate::chain;
use crate::error::{AppError, AppResult};
use crate::mint::{encode_word_address, encode_word_uint};
use alloy::primitives::U256;

const SELECTOR_BALANCE_OF: &str = "0x70a08231";
const SELECTOR_DECIMALS: &str = "0x313ce567";
const SELECTOR_SYMBOL: &str = "0x95d89b41";
const SELECTOR_TRANSFER: &str = "0xa9059cbb";

fn is_address(s: &str) -> bool {
    s.len() == 42 && s.starts_with("0x") && s[2..].chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_u256_result(v: &serde_json::Value) -> AppResult<U256> {
    let hex = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing eth_call result".into()))?;
    let h = hex.trim_start_matches("0x");
    if h.is_empty() {
        return Ok(U256::ZERO);
    }
    U256::from_str_radix(h, 16).map_err(|_| AppError::Rpc("bad eth_call word".into()))
}

pub async fn balance_of(rpc_url: &str, token: &str, holder: &str) -> AppResult<U256> {
    if !is_address(token) || !is_address(holder) {
        return Err(AppError::Invalid("bad token/holder address".into()));
    }
    let data = format!(
        "{}{}",
        SELECTOR_BALANCE_OF,
        hex::encode(encode_word_address(holder)?)
    );
    let v = chain::rpc_call(
        rpc_url,
        "eth_call",
        serde_json::json!([{ "to": token, "data": data }, "latest"]),
    )
    .await?;
    parse_u256_result(&v)
}

pub async fn decimals(rpc_url: &str, token: &str) -> AppResult<u8> {
    if !is_address(token) {
        return Err(AppError::Invalid("bad token address".into()));
    }
    let v = chain::rpc_call(
        rpc_url,
        "eth_call",
        serde_json::json!([{ "to": token, "data": SELECTOR_DECIMALS }, "latest"]),
    )
    .await?;
    let n = parse_u256_result(&v)?;
    u8::try_from(n).map_err(|_| AppError::Rpc("bad decimals".into()))
}

pub async fn symbol(rpc_url: &str, token: &str) -> AppResult<String> {
    if !is_address(token) {
        return Err(AppError::Invalid("bad token address".into()));
    }
    let v = chain::rpc_call(
        rpc_url,
        "eth_call",
        serde_json::json!([{ "to": token, "data": SELECTOR_SYMBOL }, "latest"]),
    )
    .await?;
    let hex = v
        .get("result")
        .and_then(|r| r.as_str())
        .ok_or_else(|| AppError::Rpc("missing symbol".into()))?;
    decode_abi_string(hex)
}

fn decode_abi_string(hex_result: &str) -> AppResult<String> {
    let raw = hex::decode(hex_result.trim_start_matches("0x"))
        .map_err(|_| AppError::Rpc("bad symbol hex".into()))?;
    if raw.len() < 64 {
        // Some tokens return a plain bytes32 (no ABI string wrapper).
        let s = String::from_utf8_lossy(&raw);
        return Ok(s.trim_end_matches('\0').to_string());
    }
    let mut offset = 0usize;
    for chunk in raw.chunks(32) {
        if chunk.len() == 32 {
            let mut w = [0u8; 32];
            w.copy_from_slice(chunk);
            offset = usize::try_from(U256::from_be_bytes(w))
                .map_err(|_| AppError::Rpc("bad string offset".into()))?;
            break;
        }
    }
    if offset + 32 > raw.len() {
        return Err(AppError::Rpc("bad string length word".into()));
    }
    let mut lenb = [0u8; 32];
    lenb.copy_from_slice(&raw[offset..offset + 32]);
    let len = usize::try_from(U256::from_be_bytes(lenb))
        .map_err(|_| AppError::Rpc("bad string length".into()))?;
    let start = offset + 32;
    let end = (start + len).min(raw.len());
    Ok(String::from_utf8_lossy(&raw[start..end]).to_string())
}

pub fn encode_transfer(to: &str, amount: U256) -> AppResult<String> {
    // SELECTOR_TRANSFER already includes the 0x prefix.
    let mut body = String::with_capacity(8 + 64 + 64);
    body.push_str(SELECTOR_TRANSFER.trim_start_matches("0x"));
    body.push_str(&hex::encode(encode_word_address(to)?));
    body.push_str(&hex::encode(encode_word_uint(&amount.to_string())?));
    Ok(format!("0x{body}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_encoding_shape() {
        let to = "0x70997970C51812dc3A010C7d01b50e0d17dc79C8";
        let cd = encode_transfer(to, U256::from(1_000_000u64)).unwrap();
        assert!(cd.starts_with("0xa9059cbb"));
        // selector + 2 words
        assert_eq!((cd.len() - 2 - 8) % 64, 0);
        assert!(cd.to_lowercase().contains(&to[2..].to_lowercase()));
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(encode_transfer("0x123", U256::ZERO).is_err());
        assert!(!is_address("0x00"));
        assert!(is_address(
            "0x0000000000000000000000000000000000000001"
        ));
        assert!(!is_address(
            "0000000000000000000000000000000000000001"
        ));
    }

    #[test]
    fn decodes_abi_string() {
        // offset(32) + len(3) + "USDC" padded
        let mut words = Vec::new();
        words.extend_from_slice(&U256::from(32u64).to_be_bytes::<32>());
        words.extend_from_slice(&U256::from(4u64).to_be_bytes::<32>());
        let mut w = [0u8; 32];
        w[..4].copy_from_slice(b"USDC");
        words.extend_from_slice(&w);
        let s = decode_abi_string(&format!("0x{}", hex::encode(words))).unwrap();
        assert_eq!(s, "USDC");
    }
}
