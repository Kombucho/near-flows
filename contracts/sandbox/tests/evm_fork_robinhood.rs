//! StonkBrokers on Robinhood Chain, from a NEAR vault: the node library's Smart LP
//! templates (deposit USDG into the sdAMZN-FR vault, withdraw back to USDG),
//! executed on a local fork of Robinhood Chain mainnet. The vault builds and
//! signs each call; the Smart LP shares and the USDG coming back land at the
//! vault's own EVM address, the only address a template can name.
//!
//! Needs Foundry (anvil, cast); skipped without it. ROBINHOOD_RPC overrides the public RPC.

mod common;

use common::{num, signed, Fork};
use serde_json::{json, Value};

const USDG: &str = "0x5fc5360d0400a0fd4f2af552add042d716f1d168";
const SMART_LP: &str = "0x5019fca4c0193908727810209b7621a90fc9f17a"; // sdAMZN-FR
const AMZN_POOL: &str = "0x8ac92da74ab5f3b1d024dc1943ad7e15dc4179ef"; // holds USDG: funds the vault on the fork
const GWEI: u128 = 1_000_000_000;

#[tokio::test]
async fn the_vault_deposits_into_and_withdraws_from_a_stonkbrokers_smart_lp_on_a_robinhood_fork() -> anyhow::Result<()> {
    let url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    let Some(fork) = Fork::start(&url, 8550).await else { return Ok(()) };
    assert_eq!(fork.cast(&["chain-id"]), "4663", "a fork of Robinhood Chain");
    println!("Robinhood Chain fork at block {}", fork.cast(&["block-number"]));

    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&common::wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&common::wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "evm_setup").args_json(json!({"signer": signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).max_gas().transact().await?.into_result()?;
    let me = vault.view("get_evm").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's EVM address on Robinhood Chain: {me}");
    for t in common::templates("robinhood") {
        alice.call(vault.id(), "evm_template").args_json(json!({"template": t})).transact().await?.into_result()?;
    }

    // Fund it on the fork: gas money and 100 USDG (taken from the AMZN pool by impersonation).
    fork.cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]);
    fork.give(USDG, AMZN_POOL, &me, 100_000_000);
    assert_eq!(fork.balance(USDG, &me), 100_000_000);

    let base_fee = num(&fork.cast(&["base-fee"]));
    let max_fee = (base_fee * 2).max(GWEI / 100).min(GWEI);
    let mut nonce = 0u64;
    let mut call = |name: &str, values: Vec<u128>| {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
            "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 1_500_000u64});
        nonce += 1;
        a
    };
    let amount: u128 = 50_000_000; // 50 USDG

    // 1. Approve the Smart LP vault for 50 USDG.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("approve_usdg_smartlp_amzn", vec![0, amount])).max_gas().transact().await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "approve executed");

    // 2. The Smart LP's own quote (a simulated deposit), as the keeper would ask it.
    let quote = num(&fork.cast(&["call", SMART_LP, "zapDeposit(address,uint256,uint256,address)(uint256)", USDG, &amount.to_string(), "0", &me, "--from", &me]));
    println!("Smart LP quote: 50 USDG -> {:.4} sdAMZN-FR shares", quote as f64 / 1e18);

    // 3. A keeper asking for more shares than the vault mints: StonkBrokers reverts (MinShares), nothing moves.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("smartlp_amzn_deposit_usdg", vec![0, amount, quote * 2, 0])).max_gas().transact().await?;
    assert_eq!(fork.send_raw(&signed(&r)), 0, "MinShares revert");
    assert_eq!(fork.balance(USDG, &me), 100_000_000);

    // 4. Deposit with minShares = 98% of the quote (their integration guide's advice).
    let r = keeper.call(vault.id(), "evm_call").args_json(call("smartlp_amzn_deposit_usdg", vec![0, amount, quote * 98 / 100, 0])).max_gas().transact().await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "zapDeposit executed");
    let shares = fork.balance(SMART_LP, &me);
    println!("deposited 50 USDG: the vault holds {:.4} sdAMZN-FR shares", shares as f64 / 1e18);
    assert!(shares >= quote * 98 / 100);
    assert_eq!(fork.balance(USDG, &me), 50_000_000);

    // 5. Withdraw every share back to USDG; the owner's floor (shares / 1.1e12) is the least accepted.
    let r = keeper.call(vault.id(), "evm_call").args_json(call("smartlp_amzn_withdraw_usdg", vec![shares, 0, 1, 0, 0])).max_gas().transact().await?;
    let raw = signed(&r);
    assert!(raw.contains(&format!("{:064x}", shares / 1_100_000_000_000)), "the owner's floor is the minimum in the signed tx");
    assert_eq!(fork.send_raw(&raw), 1, "zapWithdraw executed");
    let back = fork.balance(USDG, &me) - 50_000_000;
    println!("withdrew all shares: {:.4} USDG back ({:.2}% round trip cost: swap fees, 10 bps retention, price impact)",
        back as f64 / 1e6, (1.0 - back as f64 / amount as f64) * 100.0);
    assert_eq!(fork.balance(SMART_LP, &me), 0);
    assert!(back >= shares / 1_100_000_000_000);
    Ok(())
}
