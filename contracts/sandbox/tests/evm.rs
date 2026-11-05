//! EVM through Chain Signatures: the vault builds Uniswap transactions itself
//! and gets them signed. The signer here is a test stand-in that really signs
//! (secp256k1, and sometimes high-s like any ECDSA signer), so we recover the
//! signer's address from each transaction exactly as an Ethereum node would.

use evmtx::*;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use near_workspaces::types::NearToken;
use serde_json::{json, Value};

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

// Uniswap on Sepolia: SwapRouter02, USDC, WETH.
const ROUTER: &str = "0x3bfa4769fb09eefc5a80d6e87c3b9c650f7ae48e";
const USDC: &str = "0x1c7d4b196cb0c7b01d743fbc6116a902379c7238";
const WETH: &str = "0xfff9976782d46cc05630d1f6ebab18b2324d6b14";
const GWEI: u128 = 1_000_000_000;

fn field<'a>(logs: &'a [String], ev: &str) -> Option<&'a String> {
    logs.iter().find(|l| l.contains(&format!(r#""event":"{ev}""#)))
}
fn get(log: &str, k: &str) -> String {
    let v: Value = serde_json::from_str(log.trim_start_matches("EVENT_JSON:")).unwrap();
    match &v["data"][0][k] { Value::String(s) => s.clone(), x => x.to_string() }
}

/// The address that signed `hash` with (r, s, v), as an Ethereum node recovers it; also checks low-s.
fn recover(hash: &[u8; 32], r: &str, s: &str, v: u8) -> Address {
    let sig = Signature::from_scalars(<[u8; 32]>::try_from(hex::decode(r).unwrap()).unwrap(), <[u8; 32]>::try_from(hex::decode(s).unwrap()).unwrap()).unwrap();
    assert!(sig.normalize_s().is_none(), "s must be low (EIP-2)");
    let key = VerifyingKey::recover_from_prehash(hash, &sig, RecoveryId::from_byte(v).unwrap()).unwrap();
    address_of(key.to_encoded_point(false).as_bytes()).unwrap()
}

#[tokio::test]
async fn the_vault_builds_and_signs_uniswap_swaps_only_within_policy() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper, mallory) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "evm_setup").args_json(json!({"signer": signer.id(), "chain_id": 11155111u64, "router": ROUTER,
        "max_fee": (50 * GWEI).to_string(), "max_tip": (3 * GWEI).to_string(), "gas_cap": 300_000u64})).max_gas().transact().await?.into_result()?;
    let cfg: Value = vault.view("get_evm").await?.json()?;
    let me_hex = cfg["address"].as_str().unwrap().to_string();
    let pk: String = signer.view("derived_public_key").args_json(json!({"path": "evm-1", "predecessor": vault.id(), "domain_id": 0})).await?.json()?;
    let me = address_of(&bs58::decode(pk.trim_start_matches("secp256k1:")).into_vec()?).unwrap();
    assert_eq!(me_hex, hex_address(&me));
    println!("vault's EVM address: {me_hex}");

    // USDC (6 dec) -> WETH (18 dec): at most 100 USDC a swap, at least 0.00025 WETH per USDC (2.5e8 wei per micro-USDC).
    alice.call(vault.id(), "evm_allow").args_json(json!({"pair": {"token_in": USDC, "token_out": WETH, "fee": 3000, "max_in": "100000000",
        "min_out_per_in_e12": "250000000000000000000"}})).transact().await?.into_result()?;
    alice.call(vault.id(), "set_limit").args_json(json!({"token": format!("evm:11155111:{USDC}"), "max": "150000000", "window_ns": 86_400_000_000_000u64}))
        .transact().await?.into_result()?;

    // 1. Approve the router: signed by the vault's key.
    let r = keeper.call(vault.id(), "evm_approve").args_json(json!({"token_in": USDC, "amount": "100000000", "nonce": 0, "max_fee": (30 * GWEI).to_string(), "max_tip": (2 * GWEI).to_string()}))
        .max_gas().transact().await?;
    let logs = r.logs().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let ev = field(&logs, "evm_signed").unwrap_or_else(|| panic!("{logs:?} {:?}", r.failures())).clone();
    let approve = Eip1559 { chain_id: 11155111, nonce: 0, max_priority_fee: 2 * GWEI, max_fee: 30 * GWEI, gas_limit: 80_000, to: parse_address(USDC).unwrap(), value: 0,
        data: abi::approve(&parse_address(ROUTER).unwrap(), 100_000_000) };
    assert_eq!(recover(&approve.signing_hash(), &get(&ev, "r"), &get(&ev, "s"), get(&ev, "v").parse()?), me, "signed by the vault");

    // 2. Swap 40 USDC; the keeper's quote asks for only 1 wei: the owner's floor (0.01 WETH) wins.
    let r = keeper.call(vault.id(), "evm_swap").args_json(json!({"token_in": USDC, "token_out": WETH, "amount_in": "40000000", "min_out": "1", "nonce": 1,
        "max_fee": (30 * GWEI).to_string(), "max_tip": (2 * GWEI).to_string(), "gas": 250_000u64})).max_gas().transact().await?;
    println!("GAS build + sign a Uniswap swap: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    let logs = r.logs().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let ev = field(&logs, "evm_signed").unwrap_or_else(|| panic!("{logs:?} {:?}", r.failures())).clone();
    let floor = 40_000_000u128 * 250_000_000_000_000_000_000 / 1_000_000_000_000;
    let swap = Eip1559 { chain_id: 11155111, nonce: 1, max_priority_fee: 2 * GWEI, max_fee: 30 * GWEI, gas_limit: 250_000, to: parse_address(ROUTER).unwrap(), value: 0,
        data: abi::exact_input_single(&parse_address(USDC).unwrap(), &parse_address(WETH).unwrap(), 3000, &me, 40_000_000, floor) };
    let (rr, ss, vv) = (get(&ev, "r"), get(&ev, "s"), get(&ev, "v").parse::<u8>()?);
    assert_eq!(recover(&swap.signing_hash(), &rr, &ss, vv), me);
    let raw = swap.signed(vv, &hex::decode(&rr)?.try_into().unwrap(), &hex::decode(&ss)?.try_into().unwrap());
    assert_eq!(get(&ev, "raw"), format!("0x{}", hex::encode(&raw)), "exactly the transaction we expect: router, vault as recipient, the floor as minimum");

    // 3. Refusals.
    let swap_args = |amount: &str, fee_gwei: u128, token_out: &str| json!({"token_in": USDC, "token_out": token_out, "amount_in": amount, "min_out": "0", "nonce": 2,
        "max_fee": (fee_gwei * GWEI).to_string(), "max_tip": GWEI.to_string(), "gas": 250_000u64});
    for (name, who, args) in [
        ("over 100 USDC", &keeper, swap_args("100000001", 30, WETH)),
        ("gas price over the cap", &keeper, swap_args("1000000", 51, WETH)),
        ("a pair nobody allowed", &keeper, swap_args("1000000", 30, "0x0000000000000000000000000000000000000bad")),
        ("a stranger", &mallory, swap_args("1000000", 30, WETH)),
    ] {
        let r = who.call(vault.id(), "evm_swap").args_json(args).max_gas().transact().await?;
        assert!(r.is_failure(), "{name}");
        assert!(!r.logs().iter().any(|l| l.contains("evm_signed")), "{name} got signed");
    }
    assert!(keeper.call(vault.id(), "evm_approve").args_json(json!({"token_in": WETH, "amount": "1", "nonce": 3, "max_fee": "1", "max_tip": "1"}))
        .max_gas().transact().await?.is_failure(), "approving a token no pair spends");
    // 4. The spending limit: 40 + 100 = 140 of 150 USDC a day; another 20 is refused.
    let ok = keeper.call(vault.id(), "evm_swap").args_json(swap_args("100000000", 30, WETH)).max_gas().transact().await?;
    assert!(ok.logs().iter().any(|l| l.contains("evm_signed")), "{:?}", ok.failures());
    let r = keeper.call(vault.id(), "evm_swap").args_json(swap_args("20000000", 30, WETH)).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit"));
    let _ = alice.transfer_near(vault.id(), NearToken::from_millinear(1)).await?;
    Ok(())
}
