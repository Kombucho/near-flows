//! A relay vault's EVM templates (nodes/evm-templates.json: Uniswap swap, Aave
//! supply and withdraw), executed for real on a local fork of Base mainnet
//! (anvil on port 9800): real Uniswap v3 and Aave contracts, no real money. The
//! NEAR side runs in the local NEAR node: the brain builds and checks each call,
//! the vault signs it with the test signer (a real secp256k1 key standing in
//! for the MPC network); the signed transactions go to `anvil --fork-url`.
//!
//! The Uniswap refusals (a keeper over-asking reverts on chain, a 1-wei quote
//! can't go under the owner's floor) are proven in relay_fork.rs
//! (a_relay_vault_swaps_on_uniswap_on_a_base_fork); here the swap only gets
//! the vault its USDC, and Aave takes it from there.
//!
//! Needs Foundry (anvil, cast). Skipped when anvil isn't installed.
//! BASE_RPC overrides the public Base endpoint.

mod common;
mod relayw;

use common::{num, signed, Fork, ANVIL_KEY};
use relayw::{pyth_feeds, relay_world, set_usd, view, x, RelayWorld, ETH_FEED, USDC_FEED};
use serde_json::{json, Value};

// Base mainnet: Uniswap QuoterV2, WETH, USDC, Aave's aUSDC (the router and pool are in the templates).
const QUOTER: &str = "0x3d4e44eb1374240ce5f1b871ab261cd16335b76a";
const AUSDC: &str = "0x4e65fe4dba92790696d040ac24aa414708f5c0ab";
const WETH: &str = "0x4200000000000000000000000000000000000006";
const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
const GWEI: u128 = 1_000_000_000;

#[tokio::test]
async fn the_vault_runs_uniswap_and_aave_templates_on_a_base_fork() -> anyhow::Result<()> {
    let url = std::env::var("BASE_RPC").unwrap_or_else(|_| "https://mainnet.base.org".into());
    let Some(fork) = Fork::start(&url, 9800).await else { return Ok(()) };
    let cast = |a: &[&str]| fork.cast(a);
    let balance = |t: &str, w: &str| fork.balance(t, w);
    let send_raw = |raw: &str| fork.send_raw(raw);
    assert_eq!(cast(&["chain-id"]), "8453", "a fork of Base mainnet");
    println!("Base fork at block {}", cast(&["block-number"]));

    // NEAR side: Alice's relay vault, set up for Base with the test signer.
    let RelayWorld { worker, brain, alice, signer, vault, .. } = relay_world().await?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 8453u64,
        "max_fee": (2 * GWEI).to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 500_000u64})).await?.into_result()?;
    let me = view(&brain, &vault, "get_evm", Value::Null).await?["address"].as_str().unwrap().to_string();
    println!("relay vault's EVM address on Base: {me}");
    // The node library's Base templates, exactly as published.
    for t in common::templates("base") {
        x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }

    // Fund it on the fork: gas money, and 0.5 WETH wrapped and sent by anvil's test account.
    cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]); // 0.1 ETH
    cast(&["send", WETH, "deposit()", "--value", "1ether", "--private-key", ANVIL_KEY]);
    cast(&["send", WETH, "transfer(address,uint256)", &me, "500000000000000000", "--private-key", ANVIL_KEY]);
    assert_eq!(balance(WETH, &me), 500_000_000_000_000_000);

    // The real pool's price right now, from Uniswap's own quoter: 0.1 WETH -> ? USDC.
    let amount: u128 = 100_000_000_000_000_000;
    let quote = num(&cast(&["call", QUOTER, "quoteExactInputSingle((address,address,uint256,uint24,uint160))(uint256,uint160,uint32,uint256)",
        &format!("({WETH},{USDC},{amount},500,0)")]));
    println!("Uniswap quote: 0.1 WETH -> {:.2} USDC", quote as f64 / 1e6);
    // The swap's market minimum reads ETH and USDC: the oracle where the pool is (USDC $1, ETH = 10 x the quote).
    let pyth = pyth_feeds(&worker, &alice, &vault, &[("eth", ETH_FEED), ("usdc", USDC_FEED)]).await?;
    set_usd(&alice, &pyth, ETH_FEED, (quote * 1_000) as i64, 0).await?;
    set_usd(&alice, &pyth, USDC_FEED, 100_000_000, 0).await?;

    let mut nonce = 0u64;
    let mut call = |name: &'static str, values: Vec<u128>| {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
            "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string(), "gas": 400_000u64});
        nonce += 1;
        a
    };

    // 1. Approve the router and swap 0.1 WETH with min_out = 99% of the quote: real Uniswap pays the vault.
    let r = x(&alice, &vault, "evm_call", call("approve_weth_uniswap", vec![0, 200_000_000_000_000_000])).await?;
    assert_eq!(send_raw(&signed(&r)), 1, "approve executed");
    let r = x(&alice, &vault, "evm_call", call("uniswap_swap_weth_usdc", vec![0, 0, 0, 0, amount, quote * 99 / 100, 0])).await?;
    assert_eq!(send_raw(&signed(&r)), 1, "swap executed");
    let usdc = balance(USDC, &me);
    println!("swapped 0.1 WETH -> {:.2} USDC into the vault's own address", usdc as f64 / 1e6);
    assert!(usdc >= quote * 99 / 100, "at least the minimum");
    assert_eq!(balance(WETH, &me), 400_000_000_000_000_000);

    // 2. Aave: lend all of it, then take it back. aUSDC is the receipt, held by the vault itself.
    let r = x(&alice, &vault, "evm_call", call("approve_usdc_aave", vec![0, usdc])).await?;
    assert_eq!(send_raw(&signed(&r)), 1);
    let r = x(&alice, &vault, "evm_call", call("aave_supply_usdc", vec![0, usdc, 0, 0])).await?;
    assert_eq!(send_raw(&signed(&r)), 1, "Aave supply executed");
    let ausdc = balance(AUSDC, &me);
    println!("supplied {:.2} USDC to Aave: vault holds {:.2} aUSDC", usdc as f64 / 1e6, ausdc as f64 / 1e6);
    assert_eq!(balance(USDC, &me), 0);
    assert!(ausdc + 2 >= usdc, "aUSDC 1:1, give or take Aave's index rounding (seen: 2 wei)");
    let r = x(&alice, &vault, "evm_call", call("aave_withdraw_usdc", vec![0, ausdc, 0])).await?;
    assert_eq!(send_raw(&signed(&r)), 1, "Aave withdraw executed");
    println!("withdrew: vault holds {:.2} USDC, {:.2} aUSDC", balance(USDC, &me) as f64 / 1e6, balance(AUSDC, &me) as f64 / 1e6);
    assert!(balance(USDC, &me) + 1 >= usdc);
    Ok(())
}
