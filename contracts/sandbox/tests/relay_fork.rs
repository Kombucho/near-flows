//! Relay vaults on real chains (local forks of mainnet): the brain builds and
//! checks, the relay vault signs with its own MPC path (test signer), the fork
//! executes. Same templates as evm_fork.rs / sol_fork.rs (nodes/*.json).
//! Base fork on port 9803 (anvil), Solana fork on 9810/9811 (surfpool, public RPC).
//! Skipped when anvil / surfpool aren't installed.

mod common;
mod relayw;

use common::{num, sol_sig, sol_template, signed, Fork, SolFork, ANVIL_KEY};
use relayw::*;
use serde_json::{json, Value};

const QUOTER: &str = "0x3d4e44eb1374240ce5f1b871ab261cd16335b76a";
const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
const GWEI: u128 = 1_000_000_000;

#[tokio::test]
async fn a_relay_vault_swaps_on_uniswap_on_a_base_fork() -> anyhow::Result<()> {
    let url = std::env::var("BASE_RPC").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let Some(fork) = Fork::start(&url, 9803).await else { return Ok(()) };
    assert_eq!(fork.cast(&["chain-id"]), "8453");
    let w = relay_world().await?;
    let r = x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64,
        "max_fee": (2 * GWEI).to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 500_000u64})).await?;
    r.into_result()?;
    let me = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("relay vault's EVM address on Base: {me}");
    for t in common::templates("base") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    fork.cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]);
    fork.cast(&["send", WETH, "deposit()", "--value", "1ether", "--private-key", ANVIL_KEY]);
    fork.cast(&["send", WETH, "transfer(address,uint256)", &me, "500000000000000000", "--private-key", ANVIL_KEY]);
    let amount: u128 = 100_000_000_000_000_000;
    let quote = num(&fork.cast(&["call", QUOTER, "quoteExactInputSingle((address,address,uint256,uint24,uint160))(uint256,uint160,uint32,uint256)",
        &format!("({WETH},{USDC},{amount},500,0)")]));
    println!("Uniswap quote: 0.1 WETH -> {:.2} USDC", quote as f64 / 1e6);
    // The swap's market minimum reads ETH and USDC: the oracle where the pool is (USDC $1, ETH = 10 x the quote).
    let pyth = pyth_feeds(&w.worker, &w.alice, &w.vault, &[("eth", ETH_FEED), ("usdc", USDC_FEED)]).await?;
    set_usd(&w.alice, &pyth, ETH_FEED, (quote * 1_000) as i64, 0).await?;
    set_usd(&w.alice, &pyth, USDC_FEED, 100_000_000, 0).await?;
    let call = |name: &str, values: Vec<u128>, nonce: u64| json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string(), "gas": 400_000u64});
    let swap = |min_out: u128| vec![0, 0, 0, 0, amount, min_out, 0];

    let r = x(&w.alice, &w.vault, "evm_call", call("approve_weth_uniswap", vec![0, 200_000_000_000_000_000], 0)).await?;
    println!("GAS relay evm_call (approve): {:.1} Tgas", tgas(&r));
    assert_eq!(fork.send_raw(&signed(&r)), 1, "approve executed");
    // Over-asking: signed, reverted by Uniswap, nothing moves.
    let r = x(&w.alice, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", swap(quote * 2), 1)).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 0, "Uniswap reverts below the minimum");
    // Fair: real Uniswap pays the relay vault's own address.
    let r = x(&w.alice, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", swap(quote * 99 / 100), 2)).await?;
    println!("GAS relay evm_call (swap): {:.1} Tgas", tgas(&r));
    let ret: Value = r.clone().json()?;
    assert_eq!(ret["raw"].as_str(), Some(signed(&r).as_str()), "the caller also gets the signed tx as the return value");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "swap executed");
    let got = fork.balance(USDC, &me);
    println!("relay vault swapped 0.1 WETH -> {:.2} USDC on the Base fork", got as f64 / 1e6);
    assert!(got >= quote * 99 / 100);
    // A 1-wei quote can't go under the market (the oracle's ETH less 1%) nor the owner's fixed 2,000 USDC per WETH;
    // the amount cap holds for the owner too.
    let r = x(&w.alice, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", swap(1), 3)).await?;
    let least = (quote * 99 / 100).max(amount * 2000 / 1_000_000_000_000);
    assert!(signed(&r).contains(&format!("{least:064x}")), "the larger of the market and the fixed floor is signed");
    let r = x(&w.alice, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", vec![0, 0, 0, 0, 10 * amount, 1, 0], 4)).await?;
    assert!(fails_with(&r, "over its cap"), "{:?}", r.failures());
    assert!(fails_with(&x(&w.alice, &w.vault, "evm_call", call("nope", vec![], 4)).await?, "no such template"));
    let stranger = w.worker.dev_create_account().await?;
    assert!(fails_with(&x(&stranger, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", swap(1), 4)).await?, "owner only"));
    assert!(fails_with(&x(&w.keeper, &w.vault, "evm_call", call("uniswap_swap_weth_usdc", swap(1), 4)).await?, "owner only"), "the keeper goes through flow nodes");

    // WHY no full-vault comparison any more: contracts/vault is gone. Its last
    // figures (2026-11-06, same setup and calls): evm_call approve 18.5 Tgas
    // (relay 23.9), swap 18.9 Tgas (relay 24.4).
    Ok(())
}

#[tokio::test]
async fn a_relay_vault_buys_a_pump_fun_coin_on_a_solana_fork() -> anyhow::Result<()> {
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public RPC only
    let Some(fork) = SolFork::start(9810).await else { return Ok(()) };
    let mint = std::env::var("PUMP_MINT").unwrap_or_else(|_| "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump".into());
    let w = relay_world().await?;
    let r = x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?;
    r.into_result()?;
    let me = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("relay vault's Solana address: {me}");
    let probe = fork.helper(&["pump-buy", &me, &mint, "50000000", "5"]);
    let per_lamport = probe["tokens"].as_str().unwrap_or_else(|| panic!("pump.fun helper: {probe}")).parse::<u64>()? / 52_500_000;
    let buy = sol_template("pump_buy", &[("mint", &mint), ("max_lamports", "100000000"), ("floor_per_lamport", &(per_lamport / 2).to_string())]);
    x(&w.alice, &w.vault, "sol_template", json!({"template": buy})).await?.into_result()?;
    x(&w.alice, &w.vault, "set_limit", json!({"token": "sol:SOL", "max": "150000000", "window_ns": 86_400_000_000_000u64})).await?.into_result()?;
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));

    let b = fork.helper(&["pump-buy", &me, &mint, "50000000", "5"]);
    let r = x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": b["message"]})).await?;
    println!("GAS relay sol_sign_tx: {:.1} Tgas", tgas(&r));
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", b["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    let held = fork.helper(&["balance", &me, &mint]);
    println!("relay vault bought on the Solana fork: {} coin units (quoted {}), tx {}", held["amount"], b["tokens"], sent["signature"]);
    assert!(held["amount"].as_str().unwrap().parse::<u128>()? >= b["tokens"].as_str().unwrap().parse::<u128>()?);
    // Refused, nothing signed: over the 0.1 SOL cap; someone else's wallet; past the spending limit.
    let big = fork.helper(&["pump-buy", &me, &mint, "120000000", "5"]);
    let r = x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": big["message"]})).await?;
    assert!(sol_sig(&r).is_none() && fails_with(&r, "over the cap"), "{:?}", r.failures());
    let other = fork.helper(&["pump-buy", "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", &mint, "10000000", "5"]);
    assert!(sol_sig(&x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": other["message"]})).await?).is_none());
    let more = fork.helper(&["pump-buy", &me, &mint, "95000000", "5"]);
    assert!(fails_with(&x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": more["message"]})).await?, "spending limit"));

    // WHY no full-vault comparison any more: contracts/vault is gone. Its last
    // figure (2026-11-06, same template, a fresh buy): sol_sign_tx 15.7 Tgas (relay 21.0).
    Ok(())
}
