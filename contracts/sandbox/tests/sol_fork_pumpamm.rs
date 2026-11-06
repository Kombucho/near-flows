//! A graduated memecoin bought and sold on the pump AMM by the vault, executed for real on a local
//! fork of Solana mainnet (surfpool): the keeper builds each swap with pump.fun's own AMM SDK
//! (which wraps SOL into the vault's own wSOL account around it), the vault checks every byte
//! against the templates published in nodes/sol-templates.json and signs (test signer: a real
//! Ed25519 key standing in for the MPC network), and the fork executes it against mainnet state.
//!
//! Needs surfpool and the keeper's node modules (scripts/keeper: npm install --ignore-scripts).
//! AMM_MINT picks the coin (graduated to its canonical pump AMM pool, priced in SOL).

mod common;

use common::{sol_sig, sol_template, wasm, SolFork};
use serde_json::{json, Value};

fn units(v: &Value, k: &str) -> u128 {
    v[k].as_str().unwrap_or_else(|| panic!("{k} in {v}")).parse().unwrap()
}

#[tokio::test]
async fn the_vault_trades_a_graduated_coin_on_the_pump_amm_on_a_solana_fork() -> anyhow::Result<()> {
    let Some(fork) = SolFork::start(9100).await else { return Ok(()) };
    // Unstable Coin (USDUC): graduated from pump.fun to its canonical pump AMM pool.
    let mint = std::env::var("AMM_MINT").unwrap_or_else(|_| "CB9dDufT3ZuQXqqSfa1c5kY935TEreyBw9XJXxHKpump".into());
    let info = &fork.helper(&["amm-info", &mint])[0];
    let pool = info["pool"].as_str().unwrap_or_else(|| panic!("no pump AMM pool: {info}")).to_string();
    assert_eq!(info["quote"], "So11111111111111111111111111111111111111112", "a SOL-priced pool");
    println!("pool {pool}: {} coin units, {} lamports", info["base"], info["quoteReserve"]);

    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "sol_setup").args_json(json!({"signer": signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32}))
        .max_gas().transact().await?.into_result()?;
    let me = vault.view("get_sol").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's Solana address: {me}");
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));

    // The owner approves the coin at today's price: at most 0.1 SOL a buy, at least half the coin
    // units per lamport the pool gives now.
    let probe = fork.helper(&["amm-buy", &me, &mint, "50000000", "5"]);
    let per = units(&probe, "out");
    println!("today: 0.05 SOL -> {per} coin units; the owner's floor: half that per lamport");
    let buy = sol_template("pump_amm_buy", &[("pool", &pool), ("mint", &mint), ("max_lamports", "100000000"),
        ("floor_num", &(per / 2).to_string()), ("floor_den", "50000000")]);
    for t in [buy, sol_template("wsol_wrap", &[]), sol_template("wsol_sync", &[]), sol_template("wsol_unwrap", &[])] {
        alice.call(vault.id(), "sol_template").args_json(json!({"template": t})).transact().await?.into_result()?;
    }
    alice.call(vault.id(), "set_limit").args_json(json!({"token": "sol:SOL", "max": "120000000", "window_ns": 86_400_000_000_000u64}))
        .transact().await?.into_result()?;

    let sign = |msg: String| {
        let (k, v) = (keeper.clone(), vault.id().clone());
        async move { k.call(&v, "sol_sign_tx").args_json(json!({"message": msg})).max_gas().transact().await }
    };
    let refused = |r: &near_workspaces::result::ExecutionFinalResult, why: &str| {
        assert!(sol_sig(r).is_none(), "signed, expected a refusal ({why})");
        assert!(format!("{:?}", r.failures()).contains(why), "expected '{why}': {:?}", r.failures());
    };

    // 1. Buy with 0.05 SOL: wrap into the vault's wSOL account, swap, unwrap what's left.
    let b = fork.helper(&["amm-buy", &me, &mint, "50000000", "5"]);
    let r = sign(b["message"].as_str().unwrap().to_string()).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("buy not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", b["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "buy not executed: {sent}");
    let held = fork.helper(&["balance", &me, &mint]);
    println!("bought on the fork: 0.05 SOL -> the vault holds {} coin units (quote {}), {} lamports left (tx {})", held["amount"], b["out"], held["lamports"], sent["signature"]);
    assert!(units(&held, "amount") >= units(&b, "out") * 95 / 100, "about the quoted amount");

    // 2. Refused, nothing signed: over the 0.1 SOL cap; too few coins for the SOL put in (the AMM's
    //    slippage raises the max SOL in: 0.03 SOL worth of coin for up to 0.075 SOL is under the
    //    owner's floor); the same buy for someone else's wallet; past the spending limit (the max
    //    in, 0.05 + 5%, is what counts: 0.0525 used of 0.12, another 0.09 + 5% is too much).
    let big = fork.helper(&["amm-buy", &me, &mint, "120000000", "5"]);
    refused(&sign(big["message"].as_str().unwrap().to_string()).await?, "over the cap");
    let cheap = fork.helper(&["amm-buy", &me, &mint, "30000000", "150"]);
    refused(&sign(cheap["message"].as_str().unwrap().to_string()).await?, "under the floor");
    let other = fork.helper(&["amm-buy", "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", &mint, "10000000", "5"]);
    assert!(sol_sig(&sign(other["message"].as_str().unwrap().to_string()).await?).is_none(), "another wallet's buy was signed");
    let more = fork.helper(&["amm-buy", &me, &mint, "90000000", "5"]);
    refused(&sign(more["message"].as_str().unwrap().to_string()).await?, "spending limit");

    // 3. Sell it all back into the vault's own wSOL account, unwrapped to the vault.
    let all = held["amount"].as_str().unwrap().to_string();
    let probe = fork.helper(&["amm-sell", &me, &mint, &all, "5"]);
    let quote = units(&probe, "out");
    let sell = sol_template("pump_amm_sell", &[("pool", &pool), ("mint", &mint), ("max_units", &all), ("floor_num", &(quote / 2).to_string()), ("floor_den", &all)]);
    alice.call(vault.id(), "sol_template").args_json(json!({"template": sell})).transact().await?.into_result()?;
    let dump = fork.helper(&["amm-sell", &me, &mint, &all, "60"]);
    refused(&sign(dump["message"].as_str().unwrap().to_string()).await?, "under the floor");
    let s = fork.helper(&["amm-sell", &me, &mint, &all, "5"]);
    let r = sign(s["message"].as_str().unwrap().to_string()).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("sell not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", s["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "sell not executed: {sent}");
    let after = fork.helper(&["balance", &me, &mint]);
    let got = after["lamports"].as_u64().unwrap() as i128 - held["lamports"].as_u64().unwrap() as i128;
    println!("sold on the fork: {all} coin units -> +{got} lamports to the vault (quote {quote}), {} coin units left (tx {})", after["amount"], sent["signature"]);
    assert_eq!(after["amount"], "0");
    assert!(got > 0 && got as u128 >= quote * 90 / 100, "the SOL came back to the vault itself");
    Ok(())
}
