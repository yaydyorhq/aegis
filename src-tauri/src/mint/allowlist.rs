use crate::error::{AppError, AppResult};
use serde::Serialize;

/// One allowlist row: address plus optional proof / signature / full calldata.
#[derive(Debug, Clone, PartialEq)]
pub struct AllowlistEntry {
    pub address: String,
    pub proof: Option<Vec<String>>,
    pub signature: Option<String>,
    pub calldata: Option<String>,
}

#[derive(Serialize, Clone)]
pub struct AllowlistEntryInfo {
    pub address: String,
    pub has_proof: bool,
    pub has_signature: bool,
    pub has_calldata: bool,
}

impl AllowlistEntry {
    pub fn info(&self) -> AllowlistEntryInfo {
        AllowlistEntryInfo {
            address: self.address.clone(),
            has_proof: self.proof.as_ref().is_some_and(|p| !p.is_empty()),
            has_signature: self
                .signature
                .as_ref()
                .is_some_and(|s| !s.is_empty() && s != "0x"),
            has_calldata: self.calldata.as_ref().is_some_and(|c| c.len() > 2),
        }
    }
}

fn norm_addr(a: &str) -> AppResult<String> {
    let t = a.trim().to_lowercase();
    if !t.starts_with("0x") || t.len() != 42 {
        return Err(AppError::Invalid(format!("bad address in allowlist: {a}")));
    }
    if !t[2..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Invalid(format!("bad address in allowlist: {a}")));
    }
    Ok(t)
}

fn norm_hex(h: &str) -> AppResult<String> {
    let t = h.trim();
    let body = t.strip_prefix("0x").unwrap_or(t);
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Invalid(format!("bad hex value: {h}")));
    }
    Ok(format!("0x{}", body.to_lowercase()))
}

fn parse_proof_list(v: &serde_json::Value) -> AppResult<Option<Vec<String>>> {
    match v {
        serde_json::Value::Array(arr) => {
            if arr.is_empty() {
                return Ok(None);
            }
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| AppError::Invalid("proof items must be hex strings".into()))?;
                let h = norm_hex(s)?;
                let body = h.trim_start_matches("0x");
                if body.len() != 64 {
                    return Err(AppError::Invalid(format!(
                        "proof leaf must be bytes32 (got {} hex chars)",
                        body.len()
                    )));
                }
                out.push(h);
            }
            Ok(Some(out))
        }
        serde_json::Value::String(s) if s.trim().is_empty() => Ok(None),
        serde_json::Value::String(s) => {
            // "0xa|0xb" or "0xa,0xb"
            let parts: Vec<&str> = if s.contains('|') {
                s.split('|').collect()
            } else {
                s.split(',').collect()
            };
            let mut out = Vec::new();
            for p in parts {
                let p = p.trim();
                if p.is_empty() {
                    continue;
                }
                let h = norm_hex(p)?;
                let body = h.trim_start_matches("0x");
                if body.len() != 64 {
                    return Err(AppError::Invalid(format!(
                        "proof leaf must be bytes32 (got {} hex chars)",
                        body.len()
                    )));
                }
                out.push(h);
            }
            if out.is_empty() {
                Ok(None)
            } else {
                Ok(Some(out))
            }
        }
        _ => Ok(None),
    }
}

fn entry_from_value(addr: &str, v: &serde_json::Value) -> AppResult<AllowlistEntry> {
    let address = norm_addr(addr)?;
    match v {
        serde_json::Value::String(s) => {
            let t = s.trim();
            // bare signature (65 bytes) vs calldata vs empty
            let body = t.trim_start_matches("0x");
            if body.len() == 130 {
                Ok(AllowlistEntry {
                    address,
                    proof: None,
                    signature: Some(norm_hex(t)?),
                    calldata: None,
                })
            } else if body.len() >= 8 && body.len() % 2 == 0 && body.len() > 130 {
                Ok(AllowlistEntry {
                    address,
                    proof: None,
                    signature: None,
                    calldata: Some(norm_hex(t)?),
                })
            } else if t.is_empty() {
                Ok(AllowlistEntry {
                    address,
                    proof: None,
                    signature: None,
                    calldata: None,
                })
            } else {
                // treat as single proof leaf or list via |
                let proof = parse_proof_list(v)?;
                Ok(AllowlistEntry {
                    address,
                    proof,
                    signature: None,
                    calldata: None,
                })
            }
        }
        serde_json::Value::Array(_) => Ok(AllowlistEntry {
            address,
            proof: parse_proof_list(v)?,
            signature: None,
            calldata: None,
        }),
        serde_json::Value::Object(map) => {
            let proof = map.get("proof").or(map.get("proofs")).map(parse_proof_list).transpose()?;
            let signature = match map.get("signature").or(map.get("sig")) {
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(norm_hex(s)?),
                _ => None,
            };
            let calldata = match map.get("calldata").or(map.get("data")) {
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => Some(norm_hex(s)?),
                _ => None,
            };
            Ok(AllowlistEntry {
                address,
                proof: proof.flatten(),
                signature,
                calldata,
            })
        }
        _ => Ok(AllowlistEntry {
            address,
            proof: None,
            signature: None,
            calldata: None,
        }),
    }
}

/// Parse allowlist from JSON (array / object map) or plain text (one address per line,
/// optional `address,proof1|proof2` CSV).
pub fn parse_allowlist(text: &str) -> AppResult<Vec<AllowlistEntry>> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err(AppError::Invalid("allowlist is empty".into()));
    }

    let mut out: Vec<AllowlistEntry> = Vec::new();

    if raw.starts_with('[') || raw.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(raw)
            .map_err(|e| AppError::Invalid(format!("allowlist JSON: {e}")))?;
        match v {
            serde_json::Value::Array(arr) => {
                for item in arr {
                    match &item {
                        serde_json::Value::String(addr) => {
                            out.push(entry_from_value(addr, &serde_json::Value::Null)?);
                        }
                        serde_json::Value::Object(map) => {
                            let addr = map
                                .get("address")
                                .or(map.get("addr"))
                                .or(map.get("wallet"))
                                .and_then(|x| x.as_str())
                                .ok_or_else(|| {
                                    AppError::Invalid("allowlist object needs address".into())
                                })?;
                            out.push(entry_from_value(addr, &item)?);
                        }
                        _ => {
                            return Err(AppError::Invalid(
                                "allowlist array items must be address strings or objects".into(),
                            ));
                        }
                    }
                }
            }
            serde_json::Value::Object(map) => {
                // Support {"allowlist": [...]} or {"entries": [...]} wrappers,
                // or a direct address→payload map.
                let inner_keys = ["allowlist", "entries", "addresses", "whitelist", "list"];
                let mut handled = false;
                for k in inner_keys {
                    if let Some(inner) = map.get(k) {
                        if inner.is_array() {
                            let arr = inner.as_array().unwrap();
                            for item in arr {
                                match item {
                                    serde_json::Value::String(addr) => {
                                        out.push(entry_from_value(
                                            addr,
                                            &serde_json::Value::Null,
                                        )?);
                                    }
                                    serde_json::Value::Object(_) => {
                                        let addr = item
                                            .get("address")
                                            .or(item.get("addr"))
                                            .and_then(|x| x.as_str())
                                            .ok_or_else(|| {
                                                AppError::Invalid(
                                                    "allowlist object needs address".into(),
                                                )
                                            })?;
                                        out.push(entry_from_value(addr, item)?);
                                    }
                                    _ => {}
                                }
                            }
                            handled = true;
                            break;
                        }
                    }
                }
                if !handled {
                    for (k, val) in map {
                        if k.starts_with("0x") || k.len() == 42 {
                            out.push(entry_from_value(&k, &val)?);
                        }
                    }
                }
            }
            _ => {
                return Err(AppError::Invalid(
                    "allowlist JSON must be array or object".into(),
                ));
            }
        }
    } else {
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            // address  OR  address,proof1|proof2  OR  address;signature
            let (addr, rest) = if let Some((a, r)) = line.split_once(',') {
                (a, Some(r))
            } else if let Some((a, r)) = line.split_once(';') {
                (a, Some(r))
            } else {
                (line, None)
            };
            let address = norm_addr(addr)?;
            let (proof, signature, calldata) = match rest {
                None => (None, None, None),
                Some(r) if r.trim().is_empty() => (None, None, None),
                Some(r) => {
                    let body = r.trim();
                    let bare = body.trim_start_matches("0x");
                    if bare.len() == 130 && bare.chars().all(|c| c.is_ascii_hexdigit()) {
                        (None, Some(norm_hex(body)?), None)
                    } else if bare.len() >= 10
                        && bare.len() % 2 == 0
                        && bare.chars().all(|c| c.is_ascii_hexdigit())
                        && !body.contains('|')
                    {
                        // long hex → full calldata
                        if bare.len() > 200 {
                            (None, None, Some(norm_hex(body)?))
                        } else {
                            (parse_proof_list(&serde_json::Value::String(body.to_string()))?, None, None)
                        }
                    } else {
                        (parse_proof_list(&serde_json::Value::String(body.to_string()))?, None, None)
                    }
                }
            };
            out.push(AllowlistEntry {
                address,
                proof,
                signature,
                calldata,
            });
        }
    }

    if out.is_empty() {
        return Err(AppError::Invalid("allowlist parsed 0 entries".into()));
    }
    // de-dupe by address (last wins)
    let mut seen = std::collections::HashMap::new();
    for e in out {
        seen.insert(e.address.clone(), e);
    }
    let mut dedup: Vec<AllowlistEntry> = seen.into_values().collect();
    dedup.sort_by(|a, b| a.address.cmp(&b.address));
    Ok(dedup)
}

pub fn find_entry<'a>(
    list: &'a [AllowlistEntry],
    address: &str,
) -> Option<&'a AllowlistEntry> {
    let key = address.trim().to_lowercase();
    list.iter().find(|e| e.address == key)
}

/// Replace `{address}` / `{signature}` in a hex calldata template.
/// Entry `calldata` (if present) wins over the template.
pub fn resolve_hex_template(
    template: &str,
    wallet_address: &str,
    entry: Option<&AllowlistEntry>,
) -> AppResult<String> {
    if let Some(e) = entry {
        if let Some(cd) = &e.calldata {
            return Ok(cd.clone());
        }
    }
    let mut out = template.to_string();
    out = out.replace("{address}", wallet_address.trim_start_matches("0x"));
    if out.contains("{signature}") {
        let sig = entry
            .and_then(|e| e.signature.as_ref())
            .ok_or_else(|| AppError::Invalid("allowlist entry missing signature".into()))?;
        out = out.replace("{signature}", sig.trim_start_matches("0x"));
    }
    if out.contains("{proof}") {
        return Err(AppError::Invalid(
            "{proof} not supported in HEX mode — use function mode with bytes32[] or per-entry calldata"
                .into(),
        ));
    }
    if !out.starts_with("0x") {
        out = format!("0x{out}");
    }
    let body = out.trim_start_matches("0x");
    if !body.len().is_multiple_of(2) || !body.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::Invalid(
            "resolved hex calldata is not valid hex".into(),
        ));
    }
    Ok(out.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_address_array() {
        let list = parse_allowlist(r#"["0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5"]"#).unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].address, "0x6152b57c981fc7d2b4d2895e8aa3ca904c42a9a5");
    }

    #[test]
    fn parses_object_with_proof() {
        let json = r#"[
            {"address":"0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5","proof":["0x1111111111111111111111111111111111111111111111111111111111111111"]}
        ]"#;
        let list = parse_allowlist(json).unwrap();
        assert_eq!(list[0].proof.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn parses_map_wrapper() {
        let json = r#"{"allowlist":["0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5","0x5EacBA6fF8368fA4Dca6D46A0aEDc484471A6BA7"]}"#;
        let list = parse_allowlist(json).unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn parses_plaintext_lines() {
        let list = parse_allowlist(
            "# comment\n0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5\n0x5EacBA6fF8368fA4Dca6D46A0aEDc484471A6BA7\n",
        )
        .unwrap();
        assert_eq!(list.len(), 2);
    }

    #[test]
    fn signature_object_entry() {
        let sig = format!("0x{}", "ab".repeat(65));
        let json = format!(
            r#"[{{"address":"0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5","signature":"{sig}"}}]"#
        );
        let list = parse_allowlist(&json).unwrap();
        assert!(list[0].signature.is_some());
        assert!(list[0].info().has_signature);
    }

    #[test]
    fn hex_template_address_and_sig() {
        let sig = format!("0x{}", "cd".repeat(65));
        let entry = AllowlistEntry {
            address: "0x6152b57c981fc7d2b4d2895e8aa3ca904c42a9a5".into(),
            proof: None,
            signature: Some(sig.clone()),
            calldata: None,
        };
        let template = "0x12345678{address}{signature}";
        let out = resolve_hex_template(
            template,
            "0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5",
            Some(&entry),
        )
        .unwrap();
        assert!(out.starts_with("0x123456786152b57c"));
        assert!(out.contains(sig.trim_start_matches("0x")));
    }

    #[test]
    fn entry_calldata_overrides_template() {
        let entry = AllowlistEntry {
            address: "0x6152b57c981fc7d2b4d2895e8aa3ca904c42a9a5".into(),
            proof: None,
            signature: None,
            calldata: Some("0xdeadbeef".into()),
        };
        let out = resolve_hex_template(
            "0x12345678{address}",
            "0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5",
            Some(&entry),
        )
        .unwrap();
        assert_eq!(out, "0xdeadbeef");
    }

    #[test]
    fn rejects_bad_address() {
        assert!(parse_allowlist("not-an-address").is_err());
    }

    #[test]
    fn find_is_case_insensitive() {
        let list = parse_allowlist("0x6152b57c981fC7d2b4d2895e8Aa3Ca904c42A9A5").unwrap();
        assert!(find_entry(&list, "0x6152B57C981FC7D2B4D2895E8AA3CA904C42A9A5").is_some());
        assert!(find_entry(&list, "0x0000000000000000000000000000000000000001").is_none());
    }
}
