//! EVM through Chain Signatures: the vault builds Uniswap transactions itself
//! and gets them signed. The signer here is a test stand-in that really signs
//! (secp256k1, and sometimes high-s like any ECDSA signer), so we recover the
//! signer's address from each transaction exactly as an Ethereum node would.
//! Run on a relay vault: the brain builds and checks, the vault asks for the signature.

mod common;
mod relayw;

use common::wasm;
use evmtx::*;
use relayw::*;
use k256::ecdsa::{RecoveryId, Signature, VerifyingKey};
use near_workspaces::types::NearToken;
use serde_json::{json, Value};

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

fn approve_t() -> Value {
    json!({"name": "approve_usdc", "to": USDC, "function": "approve(address,uint256)",
        "args": [{"rule": "fixed", "value": ROUTER}, {"rule": "at_most", "max": "100000000"}], "spends": null})
}

/// USDC (6 dec) -> WETH (18 dec): at most 100 USDC a swap, at least 0.00025 WETH per USDC (2.5e8 wei per micro-USDC).
fn swap_t() -> Value {
    json!({"name": "swap_usdc_weth", "to": ROUTER, "function": "exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))",
        "args": [{"rule": "tuple", "args": [{"rule": "fixed", "value": USDC}, {"rule": "fixed", "value": WETH}, {"rule": "fixed", "value": "3000"}, {"rule": "vault"},
                 {"rule": "at_most", "max": "100000000"}, {"rule": "floor", "of": 4, "num": "250000000", "den": "1"}, {"rule": "fixed", "value": "0"}]}],
        "spends": {"arg": 4, "token": USDC}})
}

fn call(name: &str, values: &[u128], nonce: u64, fee_gwei: u128) -> Value {
    json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
        "max_fee": (fee_gwei * GWEI).to_string(), "max_tip": (2 * GWEI).to_string(), "gas": 250_000u64})
}

#[tokio::test]
async fn the_vault_builds_and_signs_evm_calls_only_from_its_templates() -> anyhow::Result<()> {
    let RelayWorld { worker, brain, alice, keeper, signer, vault, .. } = relay_world().await?;
    let mallory = worker.dev_create_account().await?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 11155111u64,
        "max_fee": (50 * GWEI).to_string(), "max_tip": (3 * GWEI).to_string(), "gas_cap": 300_000u64})).await?.into_result()?;
    let cfg = view(&brain, &vault, "get_evm", Value::Null).await?;
    let me_hex = cfg["address"].as_str().unwrap().to_string();
    let pk: String = signer.view("derived_public_key").args_json(json!({"path": "evm-1", "predecessor": vault, "domain_id": 0})).await?.json()?;
    let me = address_of(&bs58::decode(pk.trim_start_matches("secp256k1:")).into_vec()?).unwrap();
    assert_eq!(me_hex, hex_address(&me));
    println!("vault's EVM address: {me_hex}");

    // Templates the vault refuses to store: they could take a caller's address, or don't fit their types.
    let mut bad = Vec::new();
    let mut t = approve_t(); t["args"][0] = json!({"rule": "at_most", "max": "1"}); bad.push(("a number where the spender goes", t));
    let mut t = approve_t(); t["args"][1] = json!({"rule": "vault"}); bad.push(("the vault as an amount", t));
    let mut t = approve_t(); t["function"] = json!("approve(bytes,uint256)"); bad.push(("an address rule for bytes", t));
    let mut t = approve_t(); t["function"] = json!("approve(string,uint256)"); bad.push(("a string", t));
    let mut t = approve_t(); t["value"] = json!({"rule": "fixed", "value": "1"}); bad.push(("a fixed ETH value", t));
    let mut t = swap_t(); t["args"][0]["args"][5] = json!({"rule": "same", "of": 6}); bad.push(("same as a fixed number", t));
    let mut t = approve_t(); t["args"].as_array_mut().unwrap().pop(); bad.push(("missing rule", t));
    let mut t = swap_t(); t["args"][0]["args"][2] = json!({"rule": "fixed", "value": "16777216"}); bad.push(("fee too big for uint24", t));
    let mut t = swap_t(); t["args"][0]["args"][5] = json!({"rule": "floor", "of": 2, "num": "1", "den": "1"}); bad.push(("floor relative to a fixed value", t));
    let mut t = swap_t(); t["spends"] = json!({"arg": 0, "token": USDC}); bad.push(("spends a fixed argument", t));
    for (name, t) in bad {
        assert!(x(&alice, &vault, "evm_template", json!({"template": t})).await?.is_failure(), "{name}");
    }
    assert!(x(&mallory, &vault, "evm_template", json!({"template": approve_t()})).await?.is_failure(), "only the owner sets templates");
    for t in [approve_t(), swap_t()] {
        x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&alice, &vault, "set_limit", json!({"token": format!("evm:11155111:{USDC}"), "max": "150000000", "window_ns": 86_400_000_000_000u64})).await?.into_result()?;

    // 1. Approve the router: exactly the bytes of the hand-written encoder (checked against viem), signed by the vault's key.
    let r = x(&alice, &vault, "evm_call", call("approve_usdc", &[0, 100_000_000], 0, 30)).await?;
    let logs = r.logs().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let ev = field(&logs, "evm_signed").unwrap_or_else(|| panic!("{logs:?} {:?}", r.failures())).clone();
    let approve = Eip1559 { chain_id: 11155111, nonce: 0, max_priority_fee: 2 * GWEI, max_fee: 30 * GWEI, gas_limit: 250_000, to: parse_address(USDC).unwrap(), value: 0,
        data: abi::approve(&parse_address(ROUTER).unwrap(), 100_000_000) };
    assert_eq!(recover(&approve.signing_hash(), &get(&ev, "r"), &get(&ev, "s"), get(&ev, "v").parse()?), me, "signed by the vault");

    // 2. Swap 40 USDC; a quote of only 1 wei: the owner's floor (0.01 WETH) wins.
    let r = x(&alice, &vault, "evm_call", call("swap_usdc_weth", &[0, 0, 0, 0, 40_000_000, 1, 0], 1, 30)).await?;
    println!("GAS build + sign a Uniswap swap from its template: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    let logs = r.logs().iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let ev = field(&logs, "evm_signed").unwrap_or_else(|| panic!("{logs:?} {:?}", r.failures())).clone();
    let floor = 40_000_000u128 * 250_000_000;
    let swap = Eip1559 { chain_id: 11155111, nonce: 1, max_priority_fee: 2 * GWEI, max_fee: 30 * GWEI, gas_limit: 250_000, to: parse_address(ROUTER).unwrap(), value: 0,
        data: abi::exact_input_single(&parse_address(USDC).unwrap(), &parse_address(WETH).unwrap(), 3000, &me, 40_000_000, floor) };
    let (rr, ss, vv) = (get(&ev, "r"), get(&ev, "s"), get(&ev, "v").parse::<u8>()?);
    assert_eq!(recover(&swap.signing_hash(), &rr, &ss, vv), me);
    let raw = swap.signed(vv, &hex::decode(&rr)?.try_into().unwrap(), &hex::decode(&ss)?.try_into().unwrap());
    assert_eq!(get(&ev, "raw"), format!("0x{}", hex::encode(&raw)), "exactly the transaction we expect: router, vault as recipient, the floor as minimum");

    // 3. Refusals. The keeper may not use this door at all (it reaches other chains through flow nodes).
    assert!(fails_with(&x(&keeper, &vault, "evm_call", call("swap_usdc_weth", &[0, 0, 0, 0, 1_000_000, 0, 0], 2, 30)).await?, "owner only"), "the keeper");
    for (name, who, args) in [
        ("over 100 USDC", &alice, call("swap_usdc_weth", &[0, 0, 0, 0, 100_000_001, 0, 0], 2, 30)),
        ("gas price over the cap", &alice, call("swap_usdc_weth", &[0, 0, 0, 0, 1_000_000, 0, 0], 2, 51)),
        ("a template nobody set", &alice, call("swap_usdc_scam", &[0, 0, 0, 0, 1_000_000, 0, 0], 2, 30)),
        ("wrong number of values", &alice, call("swap_usdc_weth", &[1_000_000], 2, 30)),
        ("approving more than the cap", &alice, call("approve_usdc", &[0, 100_000_001], 2, 30)),
        ("a stranger", &mallory, call("swap_usdc_weth", &[0, 0, 0, 0, 1_000_000, 0, 0], 2, 30)),
    ] {
        let r = x(&who, &vault, "evm_call", args).await?;
        assert!(r.is_failure(), "{name}");
        assert!(!r.logs().iter().any(|l| l.contains("evm_signed")), "{name} got signed");
    }
    // A "value" for a fixed or vault argument is ignored: it can't pick the recipient.
    let r = x(&alice, &vault, "evm_call", call("swap_usdc_weth", &[1, 2, 3, 0xbad, 1_000_000, 0, 9], 2, 30)).await?;
    let data = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap().to_string();
    assert!(data.contains(&hex::encode(abi::exact_input_single(&parse_address(USDC).unwrap(), &parse_address(WETH).unwrap(), 3000, &me, 1_000_000, 250_000_000_000_000))));
    // 4. The spending limit: 40 + 1 + 100 = 141 of 150 USDC a day; another 20 is refused.
    let ok = x(&alice, &vault, "evm_call", call("swap_usdc_weth", &[0, 0, 0, 0, 100_000_000, 0, 0], 3, 30)).await?;
    assert!(ok.logs().iter().any(|l| l.contains("evm_signed")), "{:?}", ok.failures());
    let r = x(&alice, &vault, "evm_call", call("swap_usdc_weth", &[0, 0, 0, 0, 20_000_000, 0, 0], 4, 30)).await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit"));

    // 5. A market floor: the minimum follows the oracle (USDC $1.0001, ETH $2,224.50, 0.5% slippage),
    //    whatever the quote. 5 USDC -> at least 0.0022366... WETH (exact, computed with Python fractions).
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let (eth_id, usdc_id) = ("ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace", "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a");
    let set = |id: &str, p: i64, age: i64| json!({"price_id": id, "price": p.to_string(), "conf": "0", "expo": -8, "backup": p.to_string(), "age_s": age});
    alice.call(pyth.id(), "set_price").args_json(set(eth_id, 222_450_000_000, 0)).transact().await?.into_result()?;
    alice.call(pyth.id(), "set_price").args_json(set(usdc_id, 100_010_000, 0)).transact().await?.into_result()?;
    x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "eth", "contract": pyth.id(), "price_id": eth_id, "backup": common::backup(pyth.id(), eth_id)},
        {"name": "usdc", "contract": pyth.id(), "price_id": usdc_id, "backup": common::backup(pyth.id(), usdc_id)}]}})).await?.into_result()?;
    let mut t = swap_t();
    t["name"] = json!("swap_usdc_weth_market");
    t["args"][0]["args"][5] = json!({"rule": "market", "of": 4, "num": "0", "den": "1",
        "market": {"sell": "usdc", "buy": "eth", "dec_in": 6, "dec_out": 18, "slippage_bps": 50}});
    x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    x(&alice, &vault, "set_limit", json!({"token": format!("evm:11155111:{USDC}"), "max": "1000000000", "window_ns": 86_400_000_000_000u64})).await?.into_result()?;
    let r = x(&alice, &vault, "evm_call", call("swap_usdc_weth_market", &[0, 0, 0, 0, 5_000_000, 1, 0], 5, 30)).await?;
    let req = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap_or_else(|| panic!("{:?} {:?}", r.logs(), r.failures())).to_string();
    assert!(req.contains(&format!("{:064x}", 2_236_681_276_691_391u128)), "the oracle floor is the minimum: {req}");
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")));
    // A stale price: nothing is signed.
    alice.call(pyth.id(), "set_price").args_json(set(eth_id, 222_450_000_000, 300)).transact().await?.into_result()?;
    let r = x(&alice, &vault, "evm_call", call("swap_usdc_weth_market", &[0, 0, 0, 0, 5_000_000, 1, 0], 6, 30)).await?;
    assert!(!r.logs().iter().any(|l| l.contains("evm_signed")) && format!("{:?}", r.failures()).contains("no price fresher"), "{:?}", r.failures());
    let _ = alice.transfer_near(&vault, NearToken::from_millinear(1)).await?;
    Ok(())
}
