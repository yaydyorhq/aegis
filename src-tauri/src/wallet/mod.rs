use crate::error::{AppError, AppResult};
use crate::vault;
use alloy::primitives::{Address, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use zeroize::Zeroizing;

pub struct NewWallet {
    pub label: String,
    pub private_key: Zeroizing<[u8; 32]>,
    pub address: Address,
}

pub fn generate(label: &str) -> AppResult<NewWallet> {
    if label.trim().is_empty() {
        return Err(AppError::Invalid("label required".into()));
    }
    let signer = PrivateKeySigner::random();
    let mut sk = Zeroizing::new([0u8; 32]);
    sk.copy_from_slice(signer.to_bytes().as_slice());
    Ok(NewWallet {
        label: label.trim().to_string(),
        private_key: sk,
        address: signer.address(),
    })
}

pub fn parse_import(label: &str, private_key: &str) -> AppResult<NewWallet> {
    if label.trim().is_empty() {
        return Err(AppError::Invalid("label required".into()));
    }
    let raw = private_key.trim().trim_start_matches("0x");
    let bytes = hex::decode(raw).map_err(|_| AppError::Invalid("private key must be hex".into()))?;
    if bytes.len() != 32 {
        return Err(AppError::Invalid(
            "private key must be 32 bytes (64 hex chars)".into(),
        ));
    }
    let mut sk = Zeroizing::new([0u8; 32]);
    sk.copy_from_slice(&bytes);
    let b = B256::from_slice(sk.as_slice());
    let signer = PrivateKeySigner::from_bytes(&b)
        .map_err(|e| AppError::Invalid(format!("invalid private key: {e}")))?;
    if signer.address() == Address::ZERO {
        return Err(AppError::Invalid("zero address".into()));
    }
    Ok(NewWallet {
        label: label.trim().to_string(),
        private_key: sk,
        address: signer.address(),
    })
}

pub fn encrypt_privkey(sk: &[u8; 32]) -> AppResult<(Vec<u8>, Vec<u8>)> {
    vault::encrypt(sk)
}

pub fn decrypt_privkey(nonce: &[u8], ct: &[u8]) -> AppResult<Zeroizing<[u8; 32]>> {
    let pt = vault::decrypt(nonce, ct)?;
    if pt.len() != 32 {
        return Err(AppError::Crypto("bad key length after decrypt".into()));
    }
    let mut sk = Zeroizing::new([0u8; 32]);
    sk.copy_from_slice(&pt);
    Ok(sk)
}

pub fn load_signer(nonce: &[u8], ct: &[u8]) -> AppResult<PrivateKeySigner> {
    let sk = decrypt_privkey(nonce, ct)?;
    let b = B256::from_slice(sk.as_slice());
    PrivateKeySigner::from_bytes(&b).map_err(|e| AppError::Crypto(format!("signer: {e}")))
}

pub fn to_checksum_str(addr: &Address) -> String {
    addr.to_checksum(None)
}

pub fn balance_to_eth(wei: U256) -> String {
    let frac = wei % U256::from(1_000_000_000_000_000_000u64);
    let whole = wei / U256::from(1_000_000_000_000_000_000u64);
    let frac_s = format!("{:018}", frac);
    let trimmed = frac_s.trim_end_matches('0');
    if trimmed.is_empty() {
        format!("{}", whole)
    } else {
        format!("{}.{}", whole, trimmed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_known_key_yields_expected_address() {
        // well-known test key
        let key = "0x0000000000000000000000000000000000000000000000000000000000000001";
        let w = parse_import("t", key).unwrap();
        assert_eq!(
            w.address.to_checksum(None).to_lowercase(),
            "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf"
        );
    }

    #[test]
    fn reject_short_key() {
        assert!(parse_import("t", "0xdeadbeef").is_err());
    }

    #[test]
    fn generate_produces_unique() {
        let a = generate("a").unwrap();
        let b = generate("b").unwrap();
        assert_ne!(a.address, b.address);
    }
}
