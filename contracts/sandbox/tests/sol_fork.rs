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

use common::{wasm, SolFork};
use serde_json::{json, Value};

const PUMP: &str = "6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P";
const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// pump.fun `buy` (classic): tokens out >= max_sol_cost x floor, max_sol_cost <= cap; the coin
/// lands in the vault's own token account; trailing accounts from the 2026-09 upgrade allowed.
fn pump_buy(mint: &str, max_lamports: u64, floor_per_lamport: u64) -> Value {
    let mut accounts = vec![json!({"acc": "any"}); 16];
    accounts[2] = json!({"acc": "fixed", "key": mint});
    accounts[5] = json!({"acc": "vault_ata", "mint_at": 2, "program_at": 8});
    accounts[6] = json!({"acc": "vault"});
    accounts[7] = json!({"acc": "fixed", "key": "11111111111111111111111111111111"});
    accounts[11] = json!({"acc": "fixed", "key": PUMP});
    json!({"name": "pump_buy", "program": PUMP, "prefix": "66063d1201daebea",
        "fields": [
            {"ty": "u64", "rule": "at_least", "of": 1, "num": floor_per_lamport.to_string(), "den": "1"},
            {"ty": "u64", "rule": "at_most", "max": max_lamports.to_string()},
            {"ty": "bool", "rule": "any"},
            {"ty": "bool", "rule": "fixed", "hex": "00"}],
        "accounts": accounts, "extra_accounts": 2, "needs_owner": false,
        "spends": [{"field": 1, "key": "sol:SOL"}]})
}

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
    alice.call(vault.id(), "sol_template").args_json(json!({"template": pump_buy(&mint, 100_000_000, per_lamport / 2)})).transact().await?.into_result()?;
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
    Ok(())
}
