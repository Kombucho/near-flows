//! Byte-for-byte against viem (tests/vectors/gen.cjs, viem 2.21.0).

use evmtx::*;

fn v() -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{}/tests/vectors/viem.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap()
}
fn hex(b: &[u8]) -> String {
    format!("0x{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>())
}
fn a(s: &str) -> Address {
    parse_address(&s.to_lowercase()).unwrap()
}

const ROUTER: &str = "0x3bFA4769FB09eefC5a80d6E87c3B9C650f7Ae48E";
const WETH: &str = "0xfFf9976782d46CC05630D1f6eBAb18b2324d6B14";
const USDC: &str = "0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238";
const ME: &str = "0x00000000000000000000000000000000000000ab";

fn swap_tx() -> Eip1559 {
    Eip1559 { chain_id: 11155111, nonce: 7, max_priority_fee: 1_500_000_000, max_fee: 30_000_000_000, gas_limit: 250_000, to: a(ROUTER), value: 0,
        data: abi::exact_input_single(&a(USDC), &a(WETH), 3000, &a(ME), 1_000_000, 123_456_789) }
}

#[test]
fn calls_match_viem() {
    let v = v();
    assert_eq!(hex(&abi::approve(&a(ROUTER), 1_000_000)), v["approve"]);
    assert_eq!(hex(&abi::exact_input_single(&a(USDC), &a(WETH), 3000, &a(ME), 1_000_000, 123_456_789)), v["swap"]);
}

#[test]
fn transactions_and_hash_match_viem() {
    let v = v();
    let tx = swap_tx();
    assert_eq!(hex(&tx.unsigned()), v["unsigned"]);
    assert_eq!(hex(&tx.signing_hash()), v["hash"]);
    assert_eq!(hex(&tx.signed(1, &[0x11; 32], &[0x22; 32])), v["signed"]);
    let empty = Eip1559 { nonce: 0, data: vec![], ..tx };
    assert_eq!(hex(&empty.unsigned()), v["zero_nonce_empty_data"]);
}

#[test]
fn address_from_a_public_key_matches_viem() {
    let mut pk = vec![4u8];
    pk.extend([0xab; 64]);
    assert_eq!(hex_address(&address_of(&pk).unwrap()), v()["addr_of_pub"].as_str().unwrap().to_lowercase());
}
