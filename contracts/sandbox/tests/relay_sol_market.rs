//! A Solana template with a market floor, through a relay vault: the brain
//! reads the oracle before it checks the message, and the template's minimum
//! out is at least what the input is worth at market, less slippage (the
//! Solana twin of the EVM `market` leaf). Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use relayw::*;
use serde_json::{json, Value};
use solguard::testkit::Tx;
use solguard::*;

const SOL_ID: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const USDC_ID: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
const BUY_PROGRAM: [u8; 32] = [21; 32];

#[tokio::test]
async fn a_solana_template_with_a_market_floor_refuses_a_price_off_the_market() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let set = |id: &'static str, usd_e8: i64, age: i64| {
        let (a, p) = (w.alice.clone(), pyth.clone());
        async move {
            a.call(p.id(), "set_price").args_json(json!({"price_id": id, "price": usd_e8.to_string(), "conf": "0", "expo": -8, "ema": usd_e8.to_string(), "age_s": age}))
                .transact().await?.into_result()?;
            anyhow::Ok(())
        }
    };
    set(SOL_ID, 20_000_000_000, 0).await?; // SOL $200
    set(USDC_ID, 100_000_000, 0).await?; // USDC $1
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "sol", "contract": pyth.id(), "price_id": SOL_ID, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": pyth.id(), "price_id": USDC_ID, "backup": {"kind": "ema"}}]}})).await?.into_result()?;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me = pubkey(w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap());
    // Sell at most 1 SOL for USDC: at least 0.001 USDC per lamport... and at least the market less 1%.
    let market = json!({"sell": "sol", "buy": "usdc", "dec_in": 9, "dec_out": 6, "slippage_bps": 100});
    let template = |rule: Value| json!({"template": {"name": "sell", "program": bs58::encode(BUY_PROGRAM).into_string(), "prefix": "02",
        "fields": [{"ty": "u64", "rule": "at_most", "max": "1000000000"}, rule], "accounts": [{"acc": "vault"}], "spends": [{"field": 0, "key": "sol:SOL"}]}});
    let bad = x(&w.alice, &w.vault, "sol_template", template(json!({"ty": "u64", "rule": "market", "of": 0, "num": "1", "den": "1000",
        "market": {"sell": "sol", "buy": "usdc", "dec_in": 9, "dec_out": 6, "slippage_bps": 6000}}))).await?;
    assert!(fails_with(&bad, "slippage"), "over 50% slippage: {:?}", bad.failures());
    x(&w.alice, &w.vault, "sol_template", template(json!({"ty": "u64", "rule": "market", "of": 0, "num": "1", "den": "1000", "market": market}))).await?.into_result()?;

    let sell = |lamports: u64, min_usdc: u64| {
        let mut d = vec![2];
        d.extend(lamports.to_le_bytes());
        d.extend(min_usdc.to_le_bytes());
        use base64::Engine;
        base64::engine::general_purpose::STANDARD.encode(Tx::new(me).ix(BUY_PROGRAM, vec![me], d).bytes())
    };
    let sign = |msg: String| x(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": msg}));
    // 1 SOL at $200, 1% slippage: at least 198 USDC. 150 USDC clears the fixed floor (1 USDC) but not the market.
    let r = sign(sell(1_000_000_000, 150_000_000)).await?;
    assert!(fails_with(&r, "under the floor 198000000"), "{:?} {:?}", r.logs(), r.failures());
    let r = sign(sell(1_000_000_000, 198_000_000)).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());
    println!("GAS sol_sign_tx with a market floor: {:.1} Tgas", tgas(&r));
    // SOL fell to $150: 149 USDC is fair now.
    set(SOL_ID, 15_000_000_000, 0).await?;
    let r = sign(sell(1_000_000_000, 149_000_000)).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());
    // No fresh price, no trade.
    set(SOL_ID, 15_000_000_000, 120).await?;
    let r = sign(sell(1_000_000_000, 149_000_000)).await?;
    assert!(fails_with(&r, "no price fresher"), "{:?}", r.failures());
    // A transaction of templates without a market field needs no price, even with the market one saved.
    x(&w.alice, &w.vault, "sol_template", json!({"template": common::sol_template("wsol_wrap", &[])})).await?.into_result()?;
    let wsol = solguard::testkit::ata_of(&me, &pubkey("So11111111111111111111111111111111111111112"));
    let mut wrap = vec![2u8, 0, 0, 0];
    wrap.extend(1_000u64.to_le_bytes());
    let wrap = { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(Tx::new(me).ix(pubkey(SYSTEM_PROGRAM), vec![me, wsol], wrap).bytes()) };
    let r = sign(wrap).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());

    // The same template as a flow's remote node: the brain reads the template's feeds before it checks.
    set(SOL_ID, 15_000_000_000, 0).await?;
    let node = json!({"input": "none", "when": {"when": "every", "every_ns": 3_600_000_000_000u64, "chunk": "1"}, "next": [],
        "action": {"label": "Sell SOL at market", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "sell", "min_field": 1}, "out": "sol:USDC", "max_out": "1000000000"}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "sell", "nodes": [node]})).await?.json()?;
    let step = |msg: String| x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": {"chain": "sol", "message": msg}}));
    let r = step(sell(1_000_000_000, 100_000_000)).await?;
    assert!(fails_with(&r, "under the floor 148500000"), "{:?} {:?}", r.logs(), r.failures());
    let r = step(sell(1_000_000_000, 149_000_000)).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")) && r.logs().iter().any(|l| l.contains(r#""min_out":"149000000""#)), "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}
