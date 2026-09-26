#[test]
fn print_selectors() {
    let sigs = [
        "getPublicDrop(address)",
        "getAllowedFeeRecipients(address)",
        "mintPublic(address,address,address,uint256)",
        "mint(uint256)",
        "mint()",
    ];
    for s in sigs {
        let h = alloy::primitives::keccak256(s.as_bytes());
        println!("{s} => 0x{}", &hex::encode(&h[..4]));
    }
}
