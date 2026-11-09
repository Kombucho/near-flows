//! Hops between NEAR Intents and the vault's own addresses on other chains,
//! executed on local forks of mainnet.
//!
//! The round trip, as one flow the keeper runs: (0) a hop node sends bridged
//! tokens out of the vault's Intents account (the real verifier and the real
//! mainnet token code, hopw) with `WITHDRAW_TO:<the vault's own address>`, built
//! by the brain; the test plays the bridge and pays that address on the fork,
//! less the bridge's fee, and the keeper reports what landed; (1) a template
//! step on the other chain spends exactly that: a deposit back into the vault's
//! Intents deposit address, which the owner pinned in the node library's deposit
//! templates (nodes/evm-templates.json `intents_deposits`, nodes/sol-templates.json
//! `intents_deposit_*`). The keeper fills only the amount; the vault signs a
//! transfer to that address and to nothing else. One-off deposits outside a flow
//! are the owner's (the direct signing doors are owner only).
//!
//! Not run here: the bridge itself (its payout on the other chain and its credit
//! of the Intents account). Base fork on port 9900 (anvil, ANVIL_PORT overrides),
//! Solana fork on 9902/9903 (surfpool, public RPC, SURFPOOL_PORT overrides).
//! Skipped when anvil / surfpool aren't installed. Needs scripts/fetch-protocols.sh.

mod common;
mod hopw;
mod relayw;

use common::{intents_deposit_template, num, signed, sol_sig, sol_template, Fork, SolFork};
use hopw::*;
use relayw::*;
use serde_json::{json, Value};

const GWEI: u128 = 1_000_000_000;
const BASE_USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
/// Aave's aUSDC on Base holds the pool's USDC: a holder to impersonate on the fork.
const BASE_AUSDC: &str = "0x4e65fe4dba92790696d040ac24aa414708f5c0ab";
/// A stand-in for the vault's Intents deposit address on EVM chains (the bridge
/// gives each Intents account one address, the same on every EVM chain).
const DEPOSIT_EVM: &str = "0x00000000000000000000000000000000d3905170";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// The bridge's withdrawal fees today (bridge API supported_tokens, 2026-11-06).
const BASE_ETH_FEE: u128 = 560_000_000_000;
const SOL_FEE: u128 = 14_000;

#[tokio::test]
async fn intents_to_base_and_back_through_the_vaults_own_addresses_on_a_base_fork() -> anyhow::Result<()> {
    let url = std::env::var("BASE_RPC").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let Some(fork) = Fork::start(&url, 9900).await else { return Ok(()) };
    let w = relay_world().await?;
    let i = intents(&w).await?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64,
        "max_fee": (2 * GWEI).to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 200_000u64})).await?.into_result()?;
    let me = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let eth = intents_deposit_template("base", "intents_deposit_eth", &[("deposit_address", DEPOSIT_EVM), ("max_wei", "100000000000000000")]);
    let usdc = intents_deposit_template("base", "intents_deposit_usdc", &[("deposit_address", DEPOSIT_EVM), ("max_units", "100000000")]);
    for t in [&eth, &usdc] {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    // A plain transfer can't smuggle calldata or an argument in.
    let mut bad = eth.clone();
    bad["args"] = json!([{"rule": "vault"}]);
    assert!(fails_with(&x(&w.alice, &w.vault, "evm_template", json!({"template": bad})).await?, "no arguments"));
    fork.cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]); // 0.1 ETH
    fork.give(BASE_USDC, BASE_AUSDC, &me, 50_000_000);
    let call = |name: &str, values: Vec<u128>, nonce: u64| json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string(), "gas": 100_000u64});
    let eth_at_deposit = || num(&fork.cast(&["balance", DEPOSIT_EVM]));
    let start = eth_at_deposit();

    // 1. One-off deposits, the owner's: 0.03 ETH as a plain transfer (value only, no calldata)
    //    and 20 USDC, both to the pinned address.
    let r = x(&w.alice, &w.vault, "evm_call", call("intents_deposit_eth", vec![30_000_000_000_000_000], 0)).await?;
    println!("GAS relay evm_call (Intents deposit, ETH): {:.1} Tgas", tgas(&r));
    assert!(r.logs().iter().any(|l| l.contains(&format!(r#""to":"{DEPOSIT_EVM}","value":"30000000000000000","data":"0x""#))), "value only, to the pinned address");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "deposit executed");
    assert_eq!(eth_at_deposit() - start, 30_000_000_000_000_000);
    let r = x(&w.alice, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0, 20_000_000], 1)).await?;
    let raw = signed(&r);
    assert!(raw.contains(&DEPOSIT_EVM[2..]), "the pinned address is in the signed calldata");
    assert_eq!(fork.send_raw(&raw), 1, "USDC deposit executed");
    assert_eq!(fork.balance(BASE_USDC, DEPOSIT_EVM), 20_000_000);
    // Refused, nothing signed: the keeper and strangers at the direct door; over the caps; no
    // template. A recipient in the fixed leaf is ignored: the address is the owner's pinned one.
    let stranger = w.worker.dev_create_account().await?;
    assert!(fails_with(&x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_eth", vec![1], 2)).await?, "owner only"));
    assert!(fails_with(&x(&stranger, &w.vault, "evm_call", call("intents_deposit_eth", vec![1], 2)).await?, "owner only"));
    assert!(fails_with(&x(&w.alice, &w.vault, "evm_call", call("intents_deposit_eth", vec![100_000_000_000_000_001], 2)).await?, "over its cap"));
    assert!(fails_with(&x(&w.alice, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0, 100_000_001], 2)).await?, "over its cap"));
    assert!(fails_with(&x(&w.alice, &w.vault, "evm_call", call("send_eth", vec![1], 2)).await?, "no such template"));
    let r = x(&w.alice, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0xdead_beef, 1], 2)).await?;
    let raw = signed(&r);
    assert!(raw.contains(&DEPOSIT_EVM[2..]) && !raw.contains("deadbeef"), "the recipient stays the pinned one");
    assert_eq!(fork.send_raw(&raw), 1);

    // 2. The round trip as a flow, run by the keeper: Base ETH out of Intents to the vault's own
    //    Base address, then exactly what landed back into its Intents deposit address.
    i.bridge_in(&i.base, &w.vault, ETH / 10).await?;
    x(&w.alice, &w.vault, "intents_hop", hop_rule(i.base.id(), "evm", ETH / 20, ETH / 10_000)).await?.into_result()?;
    let nodes = json!([
        hop_node(&format!("intents:{}", i.base.id()), "evm:8453:eth", json!([{"to": 1, "bps": 10_000}])),
        {"input": "evm:8453:eth", "when": {"when": "now"}, "next": [],
         "action": {"label": "Back into Intents", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "evm", "template": "intents_deposit_eth", "amount_leaf": 0}, "out": null}}}
    ]);
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "Intents -> Base -> Intents", "nodes": nodes})).await?.json()?;
    let hop = 2 * ETH / 100;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": hop.to_string()})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": {"chain": "hop"}})).await?;
    println!("GAS relay step_remote (hop to Base): {:.1} Tgas", tgas(&r));
    assert_eq!(withdrawn(&r), Some(("ft_burn".into(), hop, format!("WITHDRAW_TO:{me}"))), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(i.mt(&w.vault, i.base.id()).await?, ETH / 10 - hop);
    // The bridge pays the vault's own address, less its fee (played here), and the keeper reports it.
    let landed = hop - BASE_ETH_FEE;
    let before = num(&fork.cast(&["balance", &me]));
    fork.cast(&["rpc", "anvil_setBalance", &me, &format!("{:#x}", before + landed)]);
    let rep = |node: u16, out: u128| {
        let (k, v) = (w.keeper.clone(), w.vault.clone());
        async move { x(&k, &v, "remote_report", json!({"flow_id": f, "node": node, "out": out.to_string()})).await }
    };
    rep(0, landed).await?.into_result()?;
    // The template step on Base: the keeper takes the vault's next nonce; the amount is the node's.
    let nonce = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["next_nonce"].as_u64().unwrap();
    let args = |nonce: u64| json!({"flow_id": f, "node": 1, "args": {"chain": "evm", "values": ["1"], "nonce": nonce, "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string(), "gas": 100_000u64}});
    assert!(fails_with(&x(&w.keeper, &w.vault, "step_remote", args(nonce + 5)).await?, "next EVM nonce"));
    let r = x(&w.keeper, &w.vault, "step_remote", args(nonce)).await?;
    println!("GAS relay step_remote (deposit back from Base): {:.1} Tgas", tgas(&r));
    assert!(r.logs().iter().any(|l| l.contains(&format!(r#""to":"{DEPOSIT_EVM}","value":"{landed}","data":"0x""#))), "exactly what landed, to the pinned address: {:?}", r.failures());
    let at = eth_at_deposit();
    assert_eq!(fork.send_raw(&signed(&r)), 1, "deposit back executed");
    assert_eq!(eth_at_deposit() - at, landed);
    rep(1, 0).await?.into_result()?;
    let l = view(&w.brain, &w.vault, "get_flow", json!({"flow_id": f})).await?["ledger"].clone();
    assert_eq!((l["nodes"][0]["inbox"].as_str(), l["nodes"][1]["inbox"].as_str()), (Some("0"), Some("0")), "{l}");
    println!("Intents -> {me} on the Base fork -> its Intents deposit address: {landed} wei round trip");
    Ok(())
}

#[tokio::test]
async fn intents_to_solana_and_back_through_the_vaults_own_addresses_on_a_solana_fork() -> anyhow::Result<()> {
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public RPC only
    let Some(fork) = SolFork::start(9902).await else { return Ok(()) };
    let w = relay_world().await?;
    let i = intents(&w).await?;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    // A stand-in for the vault's Intents deposit address on Solana, and its USDC account (made on the fork).
    let deposit = fork.helper(&["keypair"])["pubkey"].as_str().unwrap().to_string();
    let deposit_usdc = fork.helper(&["ata", &deposit, USDC])["ata"].as_str().unwrap().to_string();
    fork.rpc_call("surfnet_setTokenAccount", json!([deposit, USDC, {"amount": 0u64}, TOKEN]));
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));
    fork.rpc_call("surfnet_setTokenAccount", json!([me, USDC, {"amount": 50_000_000u64}, TOKEN]));
    for t in [sol_template("intents_deposit_sol", &[("deposit_address", &deposit), ("max_lamports", "300000000")]),
              sol_template("intents_deposit_usdc", &[("deposit_usdc_account", &deposit_usdc), ("max_units", "30000000")])] {
        x(&w.alice, &w.vault, "sol_template", json!({"template": t})).await?.into_result()?;
    }
    x(&w.alice, &w.vault, "set_limit", json!({"token": "sol:SOL", "max": "400000000", "window_ns": 86_400_000_000_000u64})).await?.into_result()?;
    let sign = |msg: &Value| {
        let (a, v, m) = (w.alice.clone(), w.vault.clone(), msg.as_str().unwrap().to_string());
        async move { x(&a, &v, "sol_sign_tx", json!({"message": m})).await }
    };
    let send = |msg: &Value, sig: &str| fork.helper(&["send", msg.as_str().unwrap(), sig]);
    let lamports = |who: &str| fork.helper(&["balance", who, USDC])["lamports"].as_u64().unwrap() as u128;

    // 1. One-off deposits, the owner's: 0.2 SOL to the pinned address, 20 USDC to its USDC account.
    let m = fork.helper(&["pay-sol", &me, &deposit, "200000000"])["message"].clone();
    let r = sign(&m).await?;
    println!("GAS relay sol_sign_tx (Intents deposit, SOL): {:.1} Tgas", tgas(&r));
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = send(&m, &sig);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    assert_eq!(lamports(&deposit), 200_000_000, "0.2 SOL at the deposit address");
    let m = fork.helper(&["pay-spl", &me, &deposit_usdc, USDC, "20000000", "6"])["message"].clone();
    let r = sign(&m).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = send(&m, &sig);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    assert_eq!(fork.helper(&["balance", &deposit, USDC])["amount"], "20000000");

    // Refused, nothing signed: the keeper at the direct door; SOL or USDC anywhere else (a fresh
    // address, the same message with the deposit address swapped for another); over the caps.
    let thief = fork.helper(&["keypair"])["pubkey"].as_str().unwrap().to_string();
    let thief_usdc = fork.helper(&["ata", &thief, USDC])["ata"].as_str().unwrap().to_string();
    fork.rpc_call("surfnet_setTokenAccount", json!([thief, USDC, {"amount": 0u64}, TOKEN]));
    let refused = |r: &near_workspaces::result::ExecutionFinalResult, why: &str| {
        assert!(sol_sig(r).is_none(), "signed, expected a refusal ({why})");
        assert!(format!("{:?}", r.failures()).contains(why), "expected '{why}': {:?}", r.failures());
    };
    let ok = fork.helper(&["pay-sol", &me, &deposit, "1000000"])["message"].clone();
    refused(&x(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": ok})).await?, "owner only");
    refused(&sign(&fork.helper(&["pay-sol", &me, &thief, "1000000"])["message"]).await?, "not allowed");
    refused(&sign(&fork.helper(&["pay-spl", &me, &thief_usdc, USDC, "1000000", "6"])["message"]).await?, "not allowed");
    refused(&sign(&fork.helper(&["swap-key", ok.as_str().unwrap(), &deposit, &thief])["message"]).await?, "not allowed");
    refused(&sign(&fork.helper(&["pay-sol", &me, &deposit, "300000001"])["message"]).await?, "over the cap");
    refused(&sign(&fork.helper(&["pay-spl", &me, &deposit_usdc, USDC, "30000001", "6"])["message"]).await?, "over the cap");

    // 2. The round trip as a flow, run by the keeper: SOL out of Intents to the vault's own
    //    Solana address, then exactly what landed back into its Intents deposit address.
    i.bridge_in(&i.sol, &w.vault, SOL).await?;
    x(&w.alice, &w.vault, "intents_hop", hop_rule(i.sol.id(), "sol", SOL / 2, SOL / 10_000)).await?.into_result()?;
    let nodes = json!([
        hop_node(&format!("intents:{}", i.sol.id()), "sol:SOL", json!([{"to": 1, "bps": 10_000}])),
        {"input": "sol:SOL", "when": {"when": "now"}, "next": [],
         "action": {"label": "Back into Intents", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "intents_deposit_sol", "amount_field": 0}, "out": null}}}
    ]);
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "Intents -> Solana -> Intents", "nodes": nodes})).await?.json()?;
    let hop = SOL / 10;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": hop.to_string()})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": {"chain": "hop"}})).await?;
    println!("GAS relay step_remote (hop to Solana): {:.1} Tgas", tgas(&r));
    assert_eq!(withdrawn(&r), Some(("ft_transfer".into(), hop, format!("WITHDRAW_TO:{me}"))), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(ft_balance(&i.sol, i.relayer.id()).await?, hop, "omni-token handed it to the bridge's relayer");
    // The bridge pays the vault's own address, less its fee (played here), and the keeper reports it.
    let landed = hop - SOL_FEE;
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": (lamports(&me) + landed) as u64}]));
    let rep = |node: u16, out: u128| {
        let (k, v) = (w.keeper.clone(), w.vault.clone());
        async move { x(&k, &v, "remote_report", json!({"flow_id": f, "node": node, "out": out.to_string()})).await }
    };
    rep(0, landed).await?.into_result()?;
    // The template step on Solana: a transfer of exactly the node's SOL to the pinned address.
    let run = |m: &Value| {
        let (k, v, m) = (w.keeper.clone(), w.vault.clone(), m.as_str().unwrap().to_string());
        async move { x(&k, &v, "step_remote", json!({"flow_id": f, "node": 1, "args": {"chain": "sol", "message": m}})).await }
    };
    assert!(fails_with(&run(&fork.helper(&["pay-sol", &me, &deposit, &(landed + 1).to_string()])["message"]).await?, "exactly"));
    assert!(fails_with(&run(&fork.helper(&["pay-sol", &me, &thief, &landed.to_string()])["message"]).await?, "not allowed"));
    let m = fork.helper(&["pay-sol", &me, &deposit, &landed.to_string()])["message"].clone();
    let r = run(&m).await?;
    println!("GAS relay step_remote (deposit back from Solana): {:.1} Tgas", tgas(&r));
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let at = lamports(&deposit);
    let sent = send(&m, &sig);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    assert_eq!(lamports(&deposit) - at, landed);
    rep(1, 0).await?.into_result()?;
    let l = view(&w.brain, &w.vault, "get_flow", json!({"flow_id": f})).await?["ledger"].clone();
    assert_eq!((l["nodes"][0]["inbox"].as_str(), l["nodes"][1]["inbox"].as_str()), (Some("0"), Some("0")), "{l}");
    println!("Intents -> {me} on the Solana fork -> its Intents deposit address {deposit}: {landed} lamports round trip");

    // The day's SOL limit counts the flow's deposit too: 0.2 + ~0.1 spent, 0.4 a day.
    refused(&sign(&fork.helper(&["pay-sol", &me, &deposit, "150000000"])["message"]).await?, "spending limit");
    Ok(())
}
