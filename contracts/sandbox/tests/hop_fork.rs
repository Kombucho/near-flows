//! Hops INTO NEAR Intents from the vault's own addresses on other chains, executed
//! on local forks of mainnet: the owner pins the vault's Intents deposit address
//! (the bridge API's `deposit_address` for the vault's account) in the node
//! library's deposit templates (nodes/evm-templates.json `intents_deposits`,
//! nodes/sol-templates.json `intents_deposit_*`); the keeper only fills the
//! amount. The relay vault signs a transfer to that address and to nothing else.
//! The bridge's crediting of the Intents account is the one thing not run here.
//! Base fork on port 9900 (anvil), Solana fork on 9902/9903 (surfpool, public RPC).
//! Skipped when anvil / surfpool aren't installed.

mod common;
mod relayw;

use common::{intents_deposit_template, num, signed, sol_sig, sol_template, Fork, SolFork};
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

#[tokio::test]
async fn a_relay_vault_deposits_eth_and_usdc_into_intents_only_at_the_pinned_address_on_a_base_fork() -> anyhow::Result<()> {
    let url = std::env::var("BASE_RPC").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let Some(fork) = Fork::start(&url, 9900).await else { return Ok(()) };
    let w = relay_world().await?;
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
    let eth_before = num(&fork.cast(&["balance", DEPOSIT_EVM]));

    // ETH: a plain transfer of 0.03 ETH to the pinned address, nothing else in it.
    let r = x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_eth", vec![30_000_000_000_000_000], 0)).await?;
    println!("GAS relay evm_call (Intents deposit, ETH): {:.1} Tgas", tgas(&r));
    let raw = signed(&r);
    assert!(r.logs().iter().any(|l| l.contains(&format!(r#""to":"{DEPOSIT_EVM}","value":"30000000000000000","data":"0x""#))), "value only, to the pinned address");
    assert_eq!(fork.send_raw(&raw), 1, "deposit executed");
    assert_eq!(num(&fork.cast(&["balance", DEPOSIT_EVM])) - eth_before, 30_000_000_000_000_000, "0.03 ETH at the vault's Intents deposit address");
    // USDC: transfer(deposit address, 20 USDC); the recipient is fixed, the keeper only fills the amount.
    let r = x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0, 20_000_000], 1)).await?;
    let raw = signed(&r);
    assert!(raw.contains(&DEPOSIT_EVM[2..]), "the pinned address is in the signed calldata");
    assert_eq!(fork.send_raw(&raw), 1, "USDC deposit executed");
    assert_eq!(fork.balance(BASE_USDC, DEPOSIT_EVM), 20_000_000);
    println!("the vault {me} deposited 0.03 ETH and 20 USDC at its Intents deposit address on the Base fork");

    // Refused, nothing signed: over the caps; a keeper's own recipient in the fixed leaf is ignored
    // (the address is the owner's: the value the keeper sends there is replaced); no template, no
    // transfer at all; a stranger.
    assert!(fails_with(&x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_eth", vec![100_000_000_000_000_001], 2)).await?, "over its cap"));
    assert!(fails_with(&x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0, 100_000_001], 2)).await?, "over its cap"));
    let r = x(&w.keeper, &w.vault, "evm_call", call("intents_deposit_usdc", vec![0xdead_beef, 1], 2)).await?;
    assert!(signed(&r).contains(&DEPOSIT_EVM[2..]) && !signed(&r).contains("deadbeef"), "the recipient stays the pinned one");
    assert!(fails_with(&x(&w.keeper, &w.vault, "evm_call", call("send_eth", vec![1], 3)).await?, "no such template"));
    let stranger = w.worker.dev_create_account().await?;
    assert!(fails_with(&x(&stranger, &w.vault, "evm_call", call("intents_deposit_eth", vec![1], 3)).await?, "owner or keeper only"));
    Ok(())
}

#[tokio::test]
async fn a_relay_vault_deposits_sol_and_usdc_into_intents_only_at_the_pinned_address_on_a_solana_fork() -> anyhow::Result<()> {
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public RPC only
    let Some(fork) = SolFork::start(9902).await else { return Ok(()) };
    let w = relay_world().await?;
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
        let (k, v, m) = (w.keeper.clone(), w.vault.clone(), msg.as_str().unwrap().to_string());
        async move { x(&k, &v, "sol_sign_tx", json!({"message": m})).await }
    };
    let send = |msg: &Value, sig: &str| fork.helper(&["send", msg.as_str().unwrap(), sig]);

    // SOL: 0.2 SOL to the pinned deposit address.
    let m = fork.helper(&["pay-sol", &me, &deposit, "200000000"])["message"].clone();
    let r = sign(&m).await?;
    println!("GAS relay sol_sign_tx (Intents deposit, SOL): {:.1} Tgas", tgas(&r));
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = send(&m, &sig);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    assert_eq!(fork.helper(&["balance", &deposit, USDC])["lamports"].as_u64(), Some(200_000_000), "0.2 SOL at the deposit address");
    // USDC: 20 USDC from the vault's own token account to the deposit address's USDC account.
    let m = fork.helper(&["pay-spl", &me, &deposit_usdc, USDC, "20000000", "6"])["message"].clone();
    let r = sign(&m).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = send(&m, &sig);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    assert_eq!(fork.helper(&["balance", &deposit, USDC])["amount"], "20000000");
    println!("the vault {me} deposited 0.2 SOL and 20 USDC at its Intents deposit address {deposit} on the Solana fork");

    // Refused, nothing signed: SOL or USDC anywhere else (a fresh address, the same message with
    // the deposit address swapped for another); over the caps; past the day's SOL limit.
    let thief = fork.helper(&["keypair"])["pubkey"].as_str().unwrap().to_string();
    let thief_usdc = fork.helper(&["ata", &thief, USDC])["ata"].as_str().unwrap().to_string();
    fork.rpc_call("surfnet_setTokenAccount", json!([thief, USDC, {"amount": 0u64}, TOKEN]));
    let refused = |r: &near_workspaces::result::ExecutionFinalResult, why: &str| {
        assert!(sol_sig(r).is_none(), "signed, expected a refusal ({why})");
        assert!(format!("{:?}", r.failures()).contains(why), "expected '{why}': {:?}", r.failures());
    };
    refused(&sign(&fork.helper(&["pay-sol", &me, &thief, "1000000"])["message"]).await?, "not allowed");
    refused(&sign(&fork.helper(&["pay-spl", &me, &thief_usdc, USDC, "1000000", "6"])["message"]).await?, "not allowed");
    let ok = fork.helper(&["pay-sol", &me, &deposit, "1000000"])["message"].clone();
    refused(&sign(&fork.helper(&["swap-key", ok.as_str().unwrap(), &deposit, &thief])["message"]).await?, "not allowed");
    refused(&sign(&fork.helper(&["pay-sol", &me, &deposit, "300000001"])["message"]).await?, "over the cap");
    refused(&sign(&fork.helper(&["pay-spl", &me, &deposit_usdc, USDC, "30000001", "6"])["message"]).await?, "over the cap");
    refused(&sign(&fork.helper(&["pay-sol", &me, &deposit, "250000000"])["message"]).await?, "spending limit");
    Ok(())
}
