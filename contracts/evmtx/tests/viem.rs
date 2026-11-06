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


#[test]
fn template_signatures_parse_into_abi_types() {
    use evmtx::sig::{args, Ty::*};
    assert_eq!(args("approve(address,uint256)"), Some(vec![Address, Uint(256)]));
    assert_eq!(
        args("exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))"),
        Some(vec![Tuple(vec![Address, Address, Uint(24), Address, Uint(256), Uint(256), Uint(160)])])
    );
    assert_eq!(args("execute(bytes,bytes[],uint256)"), Some(vec![Bytes, Array(Box::new(Bytes)), Uint(256)]));
    assert_eq!(args("claim()"), Some(vec![]));
    assert_eq!(args("f((uint256,bytes)[][],int24)"), Some(vec![Array(Box::new(Array(Box::new(Tuple(vec![Uint(256), Bytes]))))), Int(24)]));
    for bad in ["f(string)", "f(uint256[3])", "f(uint7)", "f(address", "(address)", "f(a)b(address)", "f(address,)", "f( address)", "f(address))"] {
        assert_eq!(args(bad), None, "{bad}");
    }
}

/// Reference encodings from Foundry (`cast abi-encode`), an independent encoder.
#[test]
fn abi_encoding_matches_foundry() {
    use evmtx::enc::{encode, Val::*};
    use evmtx::sig::list;
    let w = |v: u128| { let mut x = [0u8; 32]; x[16..].copy_from_slice(&v.to_be_bytes()); Word(x) };
    let a = |s: &str| { let mut x = [0u8; 32]; x[12..].copy_from_slice(&hex::decode(s).unwrap()); Word(x) };
    let e1 = encode(&list("(bytes,bytes[],uint256)").unwrap(), &[Bytes(vec![0x10]), Array(vec![Bytes(hex::decode("deadbeef").unwrap()), Bytes(vec![])]), w(123)]);
    assert_eq!(hex::encode(e1), "000000000000000000000000000000000000000000000000000000000000006000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000007b000000000000000000000000000000000000000000000000000000000000000110000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000800000000000000000000000000000000000000000000000000000000000000004deadbeef000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");
    let key = Tuple(vec![a("0000000000000000000000000000000000000000"), a("d0601ce157db5bdc3162bbac2a2c8af5320d9eec"), w(10000), w(200), a("0000000000000000000000000000000000000000")]);
    let e2 = encode(&list("((address,address,uint24,int24,address),bool,uint128,uint128,bytes)").unwrap(), &[key, w(1), w(1000), w(7), Bytes(vec![])]);
    assert_eq!(hex::encode(e2), "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000d0601ce157db5bdc3162bbac2a2c8af5320d9eec000000000000000000000000000000000000000000000000000000000000271000000000000000000000000000000000000000000000000000000000000000c80000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000100000000000000000000000000000000000000000000000000000000000003e8000000000000000000000000000000000000000000000000000000000000000700000000000000000000000000000000000000000000000000000000000001200000000000000000000000000000000000000000000000000000000000000000");
    let e3 = encode(&list("((uint256,bytes)[],bytes,(uint8,bool))").unwrap(), &[
        Array(vec![Tuple(vec![w(1), Bytes(vec![1, 2])]), Tuple(vec![w(2), Bytes(vec![])])]),
        Bytes(hex::decode("112233445566778899aabbccddeeff00112233445566778899aabbccddeeff0011").unwrap()),
        Tuple(vec![w(5), w(0)]),
    ]);
    assert_eq!(hex::encode(e3), "000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000001c0000000000000000000000000000000000000000000000000000000000000000500000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000c000000000000000000000000000000000000000000000000000000000000000010000000000000000000000000000000000000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000000201020000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000002000000000000000000000000000000000000000000000000000000000000004000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000021112233445566778899aabbccddeeff00112233445566778899aabbccddeeff001100000000000000000000000000000000000000000000000000000000000000");
}
