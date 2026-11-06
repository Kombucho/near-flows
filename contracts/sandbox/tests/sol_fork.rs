//! Solana templates, executed for real on a local fork of Solana mainnet
//! (surfpool): the vault approves a pump.fun template, the keeper builds a real
//! buy of a coin live on its bonding curve with pump.fun's own SDK, the vault
//! checks every byte and signs (test signer: a real Ed25519 key standing in for
//! the MPC network), and the fork executes it against mainnet state.
//!
//! Needs surfpool and the keeper's node modules (scripts/keeper: npm install).
//! PUMP_MINT picks the coin (still on its bonding curve, priced in SOL: since the
//! 2026-09 upgrade some pump.fun coins are priced in USDC or other coins).

mod common;

use common::{sol_template, wasm, SolFork};
use serde_json::{json, Value};

const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

#[tokio::test]
async fn the_vault_buys_a_pump_fun_coin_on_a_solana_fork() -> anyhow::Result<()> {
    let Some(fork) = SolFork::start(9100).await else { return Ok(()) };
    let mint = std::env::var("PUMP_MINT").unwrap_or_else(|_| "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump".into());

    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "sol_setup").args_json(json!({"signer": signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32}))
        .max_gas().transact().await?.into_result()?;
    let me = vault.view("get_sol").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("vault's Solana address: {me}");
    // The owner approves the coin at today's price: at most 0.1 SOL a buy, at least half the
    // coin units per lamport the curve gives now (memecoins move tens of % in minutes).
    let probe = fork.helper(&["pump-buy", &me, &mint, "50000000", "5"]);
    let per_lamport = probe["tokens"].as_str().unwrap().parse::<u64>()? / 52_500_000;
    println!("today: {per_lamport} coin units per lamport; the owner's floor: {}", per_lamport / 2);
    // The node library's pump.fun buy (nodes/sol-templates.json), with this coin and the owner's numbers.
    let buy = sol_template("pump_buy", &[("mint", &mint), ("max_lamports", "100000000"), ("floor_per_lamport", &(per_lamport / 2).to_string())]);
    alice.call(vault.id(), "sol_template").args_json(json!({"template": buy})).transact().await?.into_result()?;
    alice.call(vault.id(), "set_limit").args_json(json!({"token": "sol:SOL", "max": "150000000", "window_ns": 86_400_000_000_000u64})).transact().await?.into_result()?;

    // 1 SOL for the vault on the fork.
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));

    let sign = |msg: String| {
        let (k, v) = (keeper.clone(), vault.id().clone());
        async move { k.call(&v, "sol_sign_tx").args_json(json!({"message": msg})).max_gas().transact().await }
    };
    let sig_of = |r: &near_workspaces::result::ExecutionFinalResult| -> Option<String> {
        r.logs().iter().find(|l| l.contains("sol_signed")).map(|l| l.split(r#""signature":""#).nth(1).unwrap().split('"').next().unwrap().to_string())
    };

    // 1. The keeper builds a real buy with pump.fun's SDK: 0.05 SOL, 5% slippage.
    let b = fork.helper(&["pump-buy", &me, &mint, "50000000", "5"]);
    println!("pump.fun quote: 0.05 SOL -> {} coin units ({})", b["tokens"], b["tokenProgram"]);
    let r = sign(b["message"].as_str().unwrap().to_string()).await?;
    let sig = sig_of(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", b["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    let held = fork.helper(&["balance", &me, &mint]);
    println!("bought on the fork: the vault holds {} coin units, {} lamports left (tx {})", held["amount"], held["lamports"], sent["signature"]);
    assert!(held["amount"].as_str().unwrap().parse::<u128>()? >= b["tokens"].as_str().unwrap().parse::<u128>()?, "at least the quoted amount");
    assert_eq!(b["tokenProgram"], TOKEN_2022);

    // 2. Refusals, nothing signed: over the 0.1 SOL cap; a buy for someone else's wallet.
    let big = fork.helper(&["pump-buy", &me, &mint, "120000000", "5"]);
    let r = sign(big["message"].as_str().unwrap().to_string()).await?;
    assert!(sig_of(&r).is_none() && format!("{:?}", r.failures()).contains("over the cap"), "{:?}", r.failures());
    let other = fork.helper(&["pump-buy", "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", &mint, "10000000", "5"]);
    let r = sign(other["message"].as_str().unwrap().to_string()).await?;
    assert!(sig_of(&r).is_none(), "another wallet's buy was signed");
    // 3. The spending limit counts the SOL read from the message: 0.0525 used of 0.15; another 0.1 is refused.
    let more = fork.helper(&["pump-buy", &me, &mint, "95000000", "5"]);
    let r = sign(more["message"].as_str().unwrap().to_string()).await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit"), "{:?}", r.failures());

    // 4. Sell it all back. The owner's floor: half the SOL per coin unit the curve pays now.
    let units = held["amount"].as_str().unwrap().to_string();
    let probe = fork.helper(&["pump-sell", &me, &mint, &units, "5"]);
    let quote = probe["lamports"].as_str().unwrap().parse::<u128>()?;
    println!("today: {units} coin units -> {quote} lamports; the owner's floor: half of that");
    let sell = sol_template("pump_sell", &[("mint", &mint), ("max_units", &units), ("floor_num", &(quote / 2).to_string()), ("floor_den", &units)]);
    alice.call(vault.id(), "sol_template").args_json(json!({"template": sell})).transact().await?.into_result()?;
    let failures = |r: &near_workspaces::result::ExecutionFinalResult| format!("{:?}", r.failures());
    // Refused: a minimum out under the owner's floor (60% slippage); more coin than the cap.
    let cheap = fork.helper(&["pump-sell", &me, &mint, &units, "60"]);
    let r = sign(cheap["message"].as_str().unwrap().to_string()).await?;
    assert!(sig_of(&r).is_none() && failures(&r).contains("under the floor"), "{}", failures(&r));
    let more = (units.parse::<u128>()? + 1).to_string();
    let over = fork.helper(&["pump-sell", &me, &mint, &more, "5"]);
    let r = sign(over["message"].as_str().unwrap().to_string()).await?;
    assert!(sig_of(&r).is_none() && failures(&r).contains("over the cap"), "{}", failures(&r));

    let s = fork.helper(&["pump-sell", &me, &mint, &units, "5"]);
    let r = sign(s["message"].as_str().unwrap().to_string()).await?;
    let sig = sig_of(&r).unwrap_or_else(|| panic!("sell not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = fork.helper(&["send", s["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "sell not executed: {sent}");
    let after = fork.helper(&["balance", &me, &mint]);
    let got = after["lamports"].as_u64().unwrap() as i128 - held["lamports"].as_u64().unwrap() as i128;
    println!("sold on the fork: {units} coin units -> the vault holds {} coin units and {} lamports (+{got} after fees; quote {quote}) (tx {})",
        after["amount"], after["lamports"], sent["signature"]);
    assert_eq!(after["amount"], "0");
    assert!(got > 0 && got as u128 >= quote * 90 / 100, "the SOL came back to the vault itself");
    Ok(())
}
