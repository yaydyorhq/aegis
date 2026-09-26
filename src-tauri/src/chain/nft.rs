use crate::chain;
use crate::error::{AppError, AppResult};
use serde::Serialize;

/// ERC-721 Transfer(address,address,uint256)
pub const ERC721_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

#[derive(Serialize, Clone)]
pub struct NftItem {
    pub id: i64,
    pub wallet_id: Option<i64>,
    pub chain_id: i64,
    pub contract: String,
    pub token_id: String,
    pub uri: Option<String>,
    pub image: Option<String>,
    pub name: Option<String>,
    pub opensea_url: Option<String>,
    pub fetched_at: i64,
}

fn addr_topic(address: &str) -> String {
    let a = address.trim().trim_start_matches("0x").to_lowercase();
    format!("0x{:0>64}", a)
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipEvent {
    pub contract: String,
    pub token_id: String,
    pub to: String,
    pub from: String,
    pub block_number: u64,
    pub log_index: u64,
    pub tx_hash: String,
}

/// Pure parser for ERC-721 Transfer logs (3 topics + no data).
pub fn parse_erc721_logs(logs: &[serde_json::Value]) -> Vec<OwnershipEvent> {
    let mut out = Vec::new();
    for log in logs {
        let topics = match log.get("topics").and_then(|t| t.as_array()) {
            Some(t) => t,
            None => continue,
        };
        if topics.len() < 4 {
            continue;
        }
        let topic0 = topics[0].as_str().unwrap_or("");
        if topic0 != ERC721_TRANSFER_TOPIC {
            continue;
        }
        let contract = log
            .get("address")
            .and_then(|a| a.as_str())
            .unwrap_or("")
            .to_lowercase();
        let from = topic_to_addr(topics[1].as_str().unwrap_or(""));
        let to = topic_to_addr(topics[2].as_str().unwrap_or(""));
        let token_id = topic_to_dec(topics[3].as_str().unwrap_or(""));
        let block = log
            .get("blockNumber")
            .and_then(|b| b.as_str())
            .and_then(|b| u64::from_str_radix(b.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let log_index = log
            .get("logIndex")
            .and_then(|b| b.as_str())
            .and_then(|b| u64::from_str_radix(b.trim_start_matches("0x"), 16).ok())
            .unwrap_or(0);
        let tx_hash = log
            .get("transactionHash")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_lowercase();
        out.push(OwnershipEvent {
            contract,
            token_id,
            to,
            from,
            block_number: block,
            log_index,
            tx_hash,
        });
    }
    out
}

fn topic_to_addr(topic: &str) -> String {
    let t = topic.trim_start_matches("0x");
    if !t.chars().all(|c| c.is_ascii_hexdigit()) {
        return "0x".into();
    }
    if t.len() >= 40 {
        format!("0x{}", t[t.len() - 40..].to_lowercase())
    } else {
        format!("0x{t}")
    }
}

fn topic_to_dec(topic: &str) -> String {
    let t = topic.trim_start_matches("0x");
    // Prefer U256 so token IDs ≥ 2^128 are preserved as decimal strings.
    match alloy::primitives::U256::from_str_radix(t, 16) {
        Ok(v) => v.to_string(),
        Err(_) => t.to_string(),
    }
}

/// Resolve last-wins ownership for a wallet address from events.
pub fn ownership_set(events: &[OwnershipEvent], wallet: &str) -> Vec<(String, String)> {
    let wallet = wallet.to_lowercase();
    use std::collections::HashMap;
    let mut map: HashMap<(String, String), bool> = HashMap::new();
    let mut sorted: Vec<_> = events.to_vec();
    sorted.sort_by(|a, b| {
        a.block_number
            .cmp(&b.block_number)
            .then(a.log_index.cmp(&b.log_index))
    });
    for e in sorted {
        let key = (e.contract.clone(), e.token_id.clone());
        if e.to.to_lowercase() == wallet {
            map.insert(key, true);
        } else if e.from.to_lowercase() == wallet {
            map.insert(key, false);
        }
    }
    map.into_iter()
        .filter(|(_, owned)| *owned)
        .map(|((c, t), _)| (c, t))
        .collect()
}

pub async fn scan_wallet_nfts(
    wallet_address: &str,
    chain_id: i64,
    window_blocks: u64,
    on_progress: &(dyn Fn(u64, u64) + Sync),
) -> AppResult<Vec<(String, String)>> {
    let chains = crate::chain::list_chains()?;
    let chain = chains
        .into_iter()
        .find(|c| c.chain_id == chain_id)
        .ok_or_else(|| AppError::NotFound(format!("chain {chain_id}")))?;
    let latest = chain::block_number(&chain.rpc_url).await?;
    // window_blocks = 0 → full history from genesis.
    let from = if window_blocks == 0 {
        0
    } else {
        latest.saturating_sub(window_blocks)
    };
    let wallet_topic = addr_topic(wallet_address);

    // Two queries (to-wallet, from-wallet) over the whole window via the
    // shared adaptive helper: one big eth_getLogs first, halved on
    // range-limit errors. Progress = blocks scanned across both queries.
    let blocks = if latest >= from { latest - from + 1 } else { 0 };
    let prog_total = blocks.saturating_mul(2).max(2);
    let tick_in: &(dyn Fn(u64, u64) + Sync) =
        &|done: u64, _total: u64| on_progress(done, prog_total);
    let logs_in = chain::get_logs_adaptive(
        &chain.rpc_url,
        from,
        latest,
        None,
        vec![
            serde_json::json!(ERC721_TRANSFER_TOPIC),
            serde_json::Value::Null,
            serde_json::json!(wallet_topic.clone()),
        ],
        Some(tick_in),
    )
    .await?;
    let tick_out: &(dyn Fn(u64, u64) + Sync) =
        &|done: u64, _total: u64| on_progress(blocks + done, prog_total);
    let logs_out = chain::get_logs_adaptive(
        &chain.rpc_url,
        from,
        latest,
        None,
        vec![
            serde_json::json!(ERC721_TRANSFER_TOPIC),
            serde_json::json!(wallet_topic.clone()),
            serde_json::Value::Null,
        ],
        Some(tick_out),
    )
    .await?;

    let mut events = parse_erc721_logs(&logs_in);
    events.extend(parse_erc721_logs(&logs_out));

    // dedupe events by (contract, token, block, from, to)
    events.sort_by(|a, b| {
        a.block_number
            .cmp(&b.block_number)
            .then(a.log_index.cmp(&b.log_index))
            .then(a.contract.cmp(&b.contract))
            .then(a.token_id.cmp(&b.token_id))
    });
    events.dedup_by(|a, b| {
        a.block_number == b.block_number
            && a.log_index == b.log_index
            && a.contract == b.contract
            && a.token_id == b.token_id
            && a.from == b.from
            && a.to == b.to
    });

    Ok(ownership_set(&events, wallet_address))
}

pub async fn token_uri(
    rpc_url: &str,
    contract: &str,
    token_id: &str,
) -> AppResult<Option<String>> {
    // tokenURI(uint256) selector 0xc87b56dd
    let id = parse_token_id_u256(token_id)?;
    let data = format!("0xc87b56dd{:064x}", id);
    let call = serde_json::json!({
        "to": contract,
        "data": data,
    });
    let v = chain::rpc_call(rpc_url, "eth_call", serde_json::json!([call, "latest"])).await?;
    let result = v
        .get("result")
        .and_then(|r| r.as_str())
        .unwrap_or("0x");
    decode_abi_string(result)
}

fn parse_token_id_u256(s: &str) -> AppResult<alloy::primitives::U256> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x") {
        alloy::primitives::U256::from_str_radix(h, 16)
            .map_err(|_| AppError::Invalid(format!("bad token id: {s}")))
    } else {
        s.parse::<u128>()
            .map(alloy::primitives::U256::from)
            .or_else(|_| alloy::primitives::U256::from_str_radix(s, 10))
            .map_err(|_| AppError::Invalid(format!("bad token id: {s}")))
    }
}

fn decode_abi_string(data: &str) -> AppResult<Option<String>> {
    let raw = data.trim_start_matches("0x");
    if raw.len() < 128 {
        return Ok(None);
    }
    // offset (32) + len (32) + bytes
    let bytes = hex::decode(raw).map_err(|_| AppError::Rpc("bad eth_call".into()))?;
    if bytes.len() < 64 {
        return Ok(None);
    }
    let mut len_bytes = [0u8; 32];
    len_bytes.copy_from_slice(&bytes[32..64]);
    // Malicious length (all 0xff or beyond buffer) — reject instead of truncating via u128.
    if len_bytes[..16].iter().any(|&b| b != 0) {
        return Ok(None);
    }
    let mut len: usize = 0;
    for &b in &len_bytes[16..] {
        len = match len.checked_mul(256).and_then(|v| v.checked_add(b as usize)) {
            Some(v) => v,
            None => return Ok(None),
        };
    }
    let end = match 64usize.checked_add(len) {
        Some(e) => e,
        None => return Ok(None),
    };
    if end > bytes.len() {
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&bytes[64..end]).to_string();
    Ok(Some(s))
}

pub fn name_image_from_json(v: &serde_json::Value) -> (Option<String>, Option<String>) {
    let name = v.get("name").and_then(|n| n.as_str()).map(|s| s.to_string());
    let image = v
        .get("image")
        .or_else(|| v.get("image_url"))
        .and_then(|n| n.as_str())
        .map(normalize_image);
    (name, image)
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const ALPH: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut rev = [0xffu8; 256];
    for (i, &c) in ALPH.iter().enumerate() {
        rev[c as usize] = i as u8;
    }
    let data: Vec<u8> = s
        .bytes()
        .filter(|b| !b.is_ascii_whitespace())
        .collect();
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u32;
    for &b in &data {
        if b == b'=' {
            break;
        }
        let v = *rev.get(b as usize)? as u32;
        if v == 0xff {
            return None;
        }
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    Some(out)
}

/// Decode `data:application/json;base64,…` token URIs into JSON.
pub fn decode_data_uri_json(uri: &str) -> Option<serde_json::Value> {
    let rest = uri.strip_prefix("data:")?;
    let (head, data) = rest.split_once(',')?;
    let head = head.to_ascii_lowercase();
    if !head.contains(";base64") || !head.contains("json") {
        return None;
    }
    let bytes = b64_decode(data)?;
    serde_json::from_slice(&bytes).ok()
}

pub async fn fetch_metadata(uri: &str) -> AppResult<(Option<String>, Option<String>)> {
    let v: serde_json::Value = if uri.starts_with("data:") {
        match decode_data_uri_json(uri) {
            Some(v) => v,
            None => return Ok((None, None)),
        }
    } else {
        let url = if let Some(rest) = uri.strip_prefix("ipfs://") {
            format!("https://ipfs.io/ipfs/{rest}")
        } else if uri.starts_with("http://") || uri.starts_with("https://") {
            uri.to_string()
        } else {
            return Ok((None, None));
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|e| AppError::Rpc(e.to_string()))?;
        let resp = client
            .get(&url)
            .send()
            .await
            .map_err(|e| AppError::Rpc(e.to_string()))?;
        if !resp.status().is_success() {
            return Ok((None, None));
        }
        resp.json().await.map_err(|e| AppError::Rpc(e.to_string()))?
    };
    Ok(name_image_from_json(&v))
}

/// OpenSea asset URL for one token: `https://opensea.io/assets/{chain}/{contract}/{id}`.
pub fn opensea_asset_url(chain_identifier: &str, contract: &str, token_id: &str) -> String {
    format!(
        "https://opensea.io/assets/{}/{}/{}",
        chain_identifier.trim().to_lowercase(),
        contract.to_lowercase(),
        token_id
    )
}

/// Fallback OpenSea chain slugs when GraphQL omits the identifier.
pub fn opensea_chain_slug(chain_id: i64) -> Option<&'static str> {
    Some(match chain_id {
        1 => "ethereum",
        8453 => "base",
        56 => "bnb",
        11155111 => "sepolia",
        57073 => "ink",
        4663 => "robinhood",
        _ => return None,
    })
}

fn normalize_image(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("ipfs://") {
        format!("https://ipfs.io/ipfs/{rest}")
    } else {
        s.to_string()
    }
}

pub type NftCacheRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub fn cache_nfts(
    wallet_id: Option<i64>,
    chain_id: i64,
    items: &[NftCacheRow],
) -> AppResult<()> {
    // (contract, token_id, uri, image, name)
    crate::db::with_conn(|conn| {
        let now = crate::db::now_ms();
        for (contract, token_id, uri, image, name, opensea_url) in items {
            // SQLite UNIQUE treats NULLs as distinct — delete matching rows first
            // so address-only scans (wallet_id = NULL) upsert correctly.
            if wallet_id.is_none() {
                conn.execute(
                    "DELETE FROM nft_cache
                     WHERE wallet_id IS NULL AND chain_id = ?1 AND contract = ?2 AND token_id = ?3",
                    rusqlite::params![chain_id, contract, token_id],
                )?;
            } else {
                conn.execute(
                    "DELETE FROM nft_cache
                     WHERE wallet_id IS ?1 AND chain_id = ?2 AND contract = ?3 AND token_id = ?4",
                    rusqlite::params![wallet_id, chain_id, contract, token_id],
                )?;
            }
            conn.execute(
                "INSERT INTO nft_cache(wallet_id, chain_id, contract, token_id, uri, image, name, opensea_url, fetched_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params![wallet_id, chain_id, contract, token_id, uri, image, name, opensea_url, now],
            )?;
        }
        Ok(())
    })
}

pub fn list_cached_nfts(wallet_id: Option<i64>) -> AppResult<Vec<NftItem>> {
    crate::db::with_conn(|conn| {
        let mut out = Vec::new();
        if let Some(wid) = wallet_id {
            let mut stmt = conn.prepare(
                "SELECT id, wallet_id, chain_id, contract, token_id, uri, image, name, opensea_url, fetched_at
                 FROM nft_cache WHERE wallet_id = ?1 ORDER BY fetched_at DESC",
            )?;
            let rows = stmt.query_map([wid], map_nft)?;
            for r in rows {
                out.push(r?);
            }
        } else {
            let mut stmt = conn.prepare(
                "SELECT id, wallet_id, chain_id, contract, token_id, uri, image, name, opensea_url, fetched_at
                 FROM nft_cache ORDER BY fetched_at DESC",
            )?;
            let rows = stmt.query_map([], map_nft)?;
            for r in rows {
                out.push(r?);
            }
        }
        Ok(out)
    })
}

fn map_nft(r: &rusqlite::Row) -> rusqlite::Result<NftItem> {
    Ok(NftItem {
        id: r.get(0)?,
        wallet_id: r.get(1)?,
        chain_id: r.get(2)?,
        contract: r.get(3)?,
        token_id: r.get(4)?,
        uri: r.get(5)?,
        image: r.get(6)?,
        name: r.get(7)?,
        opensea_url: r.get(8)?,
        fetched_at: r.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_erc721_transfer_log() {
        let log = serde_json::json!({
            "address": "0xAbCDEF0000000000000000000000000000000001",
            "topics": [
                "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
                "0x0000000000000000000000001111111111111111111111111111111111111111",
                "0x0000000000000000000000002222222222222222222222222222222222222222",
                "0x000000000000000000000000000000000000000000000000000000000000002a"
            ],
            "blockNumber": "0x10"
        });
        let events = parse_erc721_logs(&[log]);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].token_id, "42");
        assert_eq!(events[0].to, "0x2222222222222222222222222222222222222222");
        assert_eq!(events[0].block_number, 16);
    }

    #[test]
    fn ownership_last_wins() {
        let wallet = "0x2222222222222222222222222222222222222222";
        let e1 = OwnershipEvent {
            contract: "0xabc".into(),
            token_id: "1".into(),
            from: "0x0".into(),
            to: wallet.into(),
            block_number: 1,
            log_index: 0,
            tx_hash: "0x1".into(),
        };
        let e2 = OwnershipEvent {
            contract: "0xabc".into(),
            token_id: "1".into(),
            from: wallet.into(),
            to: "0x0".into(),
            block_number: 2,
            log_index: 0,
            tx_hash: "0x2".into(),
        };
        let owned = ownership_set(&[e1, e2], wallet);
        assert!(owned.is_empty());
    }

    #[test]
    fn decode_string() {
        // "hello" ABI encoded
        let data = format!(
            "0x{:064x}{:064x}{}",
            32,
            5,
            hex::encode({
                let mut b = b"hello".to_vec();
                while !b.len().is_multiple_of(32) {
                    b.push(0);
                }
                b
            })
        );
        let s = decode_abi_string(&data).unwrap();
        assert_eq!(s.as_deref(), Some("hello"));
    }

    #[test]
    fn data_uri_metadata_decode() {
        // {"name":"Genesis #1","image":"ipfs://QmX123"}
        let uri = "data:application/json;base64,eyJuYW1lIjoiR2VuZXNpcyAjMSIsImltYWdlIjoiaXBmczovL1FtWDEyMyJ9";
        let v = decode_data_uri_json(uri).expect("decodes");
        let (name, image) = name_image_from_json(&v);
        assert_eq!(name.as_deref(), Some("Genesis #1"));
        assert_eq!(image.as_deref(), Some("https://ipfs.io/ipfs/QmX123"));
        // non-json / non-base64 data URIs are rejected
        assert!(decode_data_uri_json("data:image/png;base64,iVBORw0KGgo=").is_none());
        assert!(decode_data_uri_json("https://example.com/1.json").is_none());
    }

    #[test]
    fn name_image_falls_back_to_image_url() {
        let v = serde_json::json!({ "name": "A", "image_url": "https://x/y.png" });
        let (name, image) = name_image_from_json(&v);
        assert_eq!(name.as_deref(), Some("A"));
        assert_eq!(image.as_deref(), Some("https://x/y.png"));
    }

    #[test]
    fn opensea_asset_url_format() {
        assert_eq!(
            opensea_asset_url("Robinhood", "0xEaC615689797A29394318004d0d27ff82eaA643B", "4301"),
            "https://opensea.io/assets/robinhood/0xeac615689797a29394318004d0d27ff82eaa643b/4301"
        );
        assert_eq!(opensea_chain_slug(4663), Some("robinhood"));
        assert_eq!(opensea_chain_slug(1), Some("ethereum"));
        assert_eq!(opensea_chain_slug(999999), None);
    }
}
