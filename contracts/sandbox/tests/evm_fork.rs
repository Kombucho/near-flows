//! The vault's EVM templates (nodes/evm-templates.json: Uniswap swap, Aave
//! supply and withdraw), executed for real on a local fork of Base mainnet:
//! real Uniswap v3 contracts and pools, no real money. The NEAR side runs in
//! the local NEAR node with the test signer (a real secp256k1 key standing in
//! for the MPC network); the signed transactions go to `anvil --fork-url`.
//!
//! Needs Foundry (anvil, cast). Skipped when anvil isn't installed.
//! BASE_RPC overrides the public Base endpoint.

mod common;

use common::{num, signed, Fork, ANVIL_KEY};
use near_workspaces::types::NearToken;
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
    let Some(fork) = Fork::start(&url, 8548).await else { return Ok(()) };
    let cast = |a: &[&str]| fork.cast(a);
    let balance = |t: &str, w: &str| fork.balance(t, w);
    let send_raw = |raw: &str| fork.send_raw(raw);
    assert_eq!(cast(&["chain-id"]), "8453", "a fork of Base mainnet");
    println!("Base fork at block {}", cast(&["block-number"]));

    // NEAR side: Alice's vault, set up for Base with the test signer.
    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&common::wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&common::wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "evm_setup").args_json(json!({"signer": signer.id(), "chain_id": 8453u64,
        "max_fee": (2 * GWEI).to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 500_000u64})).max_gas().transact().await?.into_result()?;
    let me = vault.view("get_evm").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's EVM address on Base: {me}");
    // The node library's Base templates, exactly as published.
    for t in common::templates("base") {
        alice.call(vault.id(), "evm_template").args_json(json!({"template": t})).transact().await?.into_result()?;
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

    let mut nonce = 0u64;
    let mut call = |name: &'static str, values: Vec<u128>| {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
            "max_fee": GWEI.to_string(), "max_tip": (GWEI / 100).to_string(), "gas": 400_000u64});
        nonce += 1;
        a
    };
    // Values: one per argument; fixed and vault arguments ignore theirs.
    let swap = |min_out: u128| vec![0, 0, 0, 0, amount, min_out, 0];

    // 1. Approve the router, signed by the vault, executed on the fork.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("approve_weth_uniswap", vec![0, 200_000_000_000_000_000])).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "approve executed");

    // 2. A keeper asking more than the pool gives (2x the quote): the vault signs it,
    //    Uniswap refuses it on chain, nothing moves (only gas).
    let r = keeper.call(vault.id(), "evm_call").args_json(call("uniswap_swap_weth_usdc", swap(quote * 2))).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 0, "Uniswap reverts below the minimum");
    assert_eq!(balance(WETH, &me), 500_000_000_000_000_000);
    assert_eq!(balance(USDC, &me), 0);

    // 3. A fair swap: min_out = 99% of the quote. Real Uniswap pays the vault.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("uniswap_swap_weth_usdc", swap(quote * 99 / 100))).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "swap executed");
    let got = balance(USDC, &me);
    println!("swapped 0.1 WETH -> {:.2} USDC into the vault's own address", got as f64 / 1e6);
    assert!(got >= quote * 99 / 100, "at least the minimum");
    assert_eq!(balance(WETH, &me), 400_000_000_000_000_000);

    // 4. A keeper quote of 1 wei can't push it below the owner's floor (2,000 USDC per WETH).
    let r = keeper.call(vault.id(), "evm_call").args_json(call("uniswap_swap_weth_usdc", swap(1))).max_gas().transact().await?;
    let raw = signed(&r);
    let floor = amount * 2000 / 1_000_000_000_000;
    assert!(raw.contains(&format!("{floor:064x}")), "minimum in the signed tx = the owner's floor");
    assert_eq!(send_raw(&raw), 1);
    let usdc = balance(USDC, &me);
    assert!(usdc >= got + floor);

    // 5. Aave: lend all of it, then take it back. aUSDC is the receipt, held by the vault itself.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("approve_usdc_aave", vec![0, usdc])).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1);
    let r = keeper.call(vault.id(), "evm_call").args_json(call("aave_supply_usdc", vec![0, usdc, 0, 0])).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "Aave supply executed");
    let ausdc = balance(AUSDC, &me);
    println!("supplied {:.2} USDC to Aave: vault holds {:.2} aUSDC", usdc as f64 / 1e6, ausdc as f64 / 1e6);
    assert_eq!(balance(USDC, &me), 0);
    assert!(ausdc + 2 >= usdc, "aUSDC 1:1, give or take Aave's index rounding (seen: 2 wei)");
    let r = keeper.call(vault.id(), "evm_call").args_json(call("aave_withdraw_usdc", vec![0, ausdc, 0])).max_gas().transact().await?;
    assert_eq!(send_raw(&signed(&r)), 1, "Aave withdraw executed");
    println!("withdrew: vault holds {:.2} USDC, {:.2} aUSDC", balance(USDC, &me) as f64 / 1e6, balance(AUSDC, &me) as f64 / 1e6);
    assert!(balance(USDC, &me) + 1 >= usdc);
    let _ = alice.transfer_near(vault.id(), NearToken::from_millinear(1)).await?;
    Ok(())
}
