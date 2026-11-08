//! The last vault modules, through relay vaults: remote flow nodes (the NVDA
//! take-profit ladder of remote.rs, same flow, same bounds), oracle market
//! floors on a NEAR recipe, and EVM template calls, with their gas. Build the
//! wasm first: make build

mod common;
mod ladder;
mod relayw;

use common::wasm;
use ladder::*;
use near_workspaces::types::NearToken;
use near_workspaces::Contract;
use relayw::*;
use serde_json::{json, Value};

fn num(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

/// Alice's relay vault on Robinhood Chain (test signer) with the node library's templates and an NVDA feed.
async fn ladder_world() -> anyhow::Result<(RelayWorld, Contract)> {
    let w = relay_world().await?;
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let r = x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?;
    println!("GAS relay evm_setup: {:.1} Tgas", tgas(&r));
    r.into_result()?;
    for t in common::templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "nvda", "contract": pyth.id(), "price_id": NVDA_FEED, "backup": common::single_source()}]}})).await?.into_result()?;
    price(&w, &pyth, 180, 0).await?;
    Ok((w, pyth))
}

async fn price(w: &RelayWorld, pyth: &Contract, usd: i64, age_s: i64) -> anyhow::Result<()> {
    let p = usd * 100_000_000;
    w.alice.call(pyth.id(), "set_price").args_json(json!({"price_id": NVDA_FEED, "price": p.to_string(), "conf": (p / 2000).to_string(),
        "expo": -8, "backup": p.to_string(), "age_s": age_s})).transact().await?.into_result()?;
    Ok(())
}

async fn ledger_of(w: &RelayWorld, f: u32) -> anyhow::Result<Value> {
    Ok(w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json::<Value>()?["ledger"].clone())
}

#[tokio::test]
async fn the_nvda_ladder_runs_on_a_relay_vault_with_the_same_bounds() -> anyhow::Result<()> {
    let (w, pyth) = ladder_world().await?;
    let addr = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("the relay vault's EVM address: {addr}");
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "NVDA ladder", "nodes": ladder()})).await?.json()?;
    let eth_in = 30_000_000_000_000_000u128;
    assert!(fails_with(&x(&w.keeper, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?, "owner only"));
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    assert!(fails_with(&x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?, "step_remote"));

    // Buy: the brain signs the node's own 0.03 ETH, a 5-minute deadline and the template's floor.
    let step = |node: u16, args: Value| x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": node, "args": args}));
    let r = step(0, evm_args(v4_values(1, true), 0)).await?;
    println!("GAS relay step_remote (buy): {:.1} Tgas", tgas(&r));
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    let signed: Value = r.clone().json()?;
    assert!(signed["raw"].as_str().unwrap().starts_with("0x02"), "the signed EIP-1559 tx comes back to the keeper: {signed}");
    let req = event(&r, "evm_call_requested");
    assert_eq!(req["value"], json!(eth_in.to_string()), "the ETH attached is the node's input");
    let s = event(&r, "remote_step");
    let min = eth_in * 1067 / 100;
    assert_eq!((num(&s["amount"]), num(&s["min_out"]), num(&s["max_out"])), (eth_in, min, eth_in * 20));

    // Bounds: busy, only the owner may fail it early, below the signed minimum, over the cap.
    assert!(fails_with(&step(0, evm_args(v4_values(1, true), 1)).await?, "Busy"));
    let report = |who: &near_workspaces::Account, node: u16, out: Option<u128>| {
        let who = who.clone();
        let v = w.vault.clone();
        async move { x(&who, &v, "remote_report", json!({"flow_id": f, "node": node, "out": out.map(|o| o.to_string())})).await }
    };
    assert!(fails_with(&report(&w.keeper, 0, None).await?, "may still land"));
    assert!(fails_with(&report(&w.keeper, 0, Some(min - 1)).await?, "below the signed minimum"));
    assert!(fails_with(&report(&w.keeper, 0, Some(eth_in * 20 + 1)).await?, "over the node's cap"));
    let got = 355_200_000_000_000_000u128;
    report(&w.keeper, 0, Some(got)).await?.into_result()?;
    let third = got * 3_333 / 10_000;
    let l = ledger_of(&w, f).await?;
    assert_eq!((inbox(&l, 1), inbox(&l, 2), inbox(&l, 3)), (third, third, third));

    // The rung fires on the oracle's price, read by the brain: not at $200, yes at $275; a stale price is no price.
    price(&w, &pyth, 200, 0).await?;
    assert!(fails_with(&step(1, evm_args(v4_values(1, false), 1)).await?, "NotYet"));
    price(&w, &pyth, 275, 0).await?;
    let r = step(1, evm_args(v4_values(1, false), 1)).await?;
    println!("GAS relay step_remote (rung, oracle read): {:.1} Tgas", tgas(&r));
    assert_eq!(num(&event(&r, "remote_price")["price"]), 275 * E24);
    report(&w.keeper, 1, Some(9_700_000_000_000_000)).await?.into_result()?;
    price(&w, &pyth, 400, 120).await?;
    assert!(fails_with(&step(2, evm_args(v4_values(1, false), 2)).await?, "no price fresher"));
    assert_eq!(inbox(&ledger_of(&w, f).await?, 2), third, "nothing taken");
    // WHY no full-vault comparison any more: contracts/vault is gone. Its last
    // figures (2026-11-06, same flow, same calls), for the record: step_remote
    // buy 22.9 Tgas (relay 28.6), rung with an oracle read 30.1 (relay 36.0).
    Ok(())
}

/// The direct signing doors skip every flow rule (trigger, inbox, the node's caps): a keeper that
/// could use them would swap at the template's fixed floor whenever it liked, and every try would
/// cost the vault a signature fee. Only the owner may; the keeper reaches other chains through nodes.
#[tokio::test]
async fn the_keeper_cannot_use_the_direct_signing_doors() -> anyhow::Result<()> {
    let w = relay_world_fees(|t| json!({"treasury": t, "admin": t, "cap_bps": 30, "bps": 10, "per_signature": (E24 / 100).to_string(), "per_signature_cap": (E24 / 20).to_string()})).await?;
    w.worker.root_account()?.create_subaccount("treasury").initial_balance(NearToken::from_near(1)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    for t in common::templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(1)).await?.into_result()?;
    let near_of = |who: near_workspaces::AccountId| {
        let worker = w.worker.clone();
        async move { anyhow::Ok(worker.view_account(&who).await?.balance.as_yoctonear()) }
    };
    let approve = json!({"name": "approve_nvda_permit2", "values": ["0", E18.to_string()], "nonce": 0, "max_fee": GWEI.to_string(), "max_tip": "0", "gas": 200_000u64});
    let before = near_of(w.vault.clone()).await?;
    for (door, args) in [
        ("evm_call", approve.clone()),
        ("sol_sign_tx", json!({"message": "AQABAg=="})),
        ("sol_sign_swap", json!({"message": "AQABAg=="})),
        ("intents_sign_swap", json!({"token_in": w.alice.id(), "token_out": w.keeper.id(), "amount_in": "1", "min_out": "1", "valid_secs": 600})),
    ] {
        let r = x(&w.keeper, &w.vault, door, args).await?;
        assert!(fails_with(&r, "owner only"), "the keeper on {door}: {:?} {:?}", r.logs(), r.failures());
        assert!(!r.logs().iter().any(|l| l.contains("_signed") || l.contains(r#""kind":"signature""#)), "{door}: nothing signed, no fee");
    }
    assert_eq!(near_of(w.vault.clone()).await?, before, "no signature fee paid for the keeper's tries");
    // The owner still may.
    let r = x(&w.alice, &w.vault, "evm_call", approve).await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR_ID: &str = "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750";
const USDC_ID: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";

#[tokio::test]
async fn a_recipe_with_a_market_floor_refuses_a_pool_pushed_off_the_market() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let minter = w.worker.dev_create_account().await?;
    let mk = |sym: &'static str| {
        let (worker, minter) = (w.worker.clone(), minter.clone());
        async move {
            let t = worker.dev_deploy(&wasm("mock_ft")).await?;
            t.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
                .transact().await?.into_result()?;
            anyhow::Ok(t)
        }
    };
    let (wnear, usdc) = (mk("wNEAR").await?, mk("USDC").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": wnear.id()})).transact().await?.into_result()?;
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [dex.id(), &w.vault] {
            minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    minter.call(usdc.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    minter.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (100 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    let set = |id: &'static str, usd_e8: i64, age: i64| {
        let (a, p) = (w.alice.clone(), pyth.clone());
        async move {
            a.call(p.id(), "set_price").args_json(json!({"price_id": id, "price": usd_e8.to_string(), "conf": "0", "expo": -8, "backup": usd_e8.to_string(), "age_s": age}))
                .transact().await?.into_result()?;
            anyhow::Ok(())
        }
    };
    set(NEAR_ID, 200_000_000, 0).await?; // NEAR $2
    set(USDC_ID, 100_000_000, 0).await?; // USDC $1
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id()}], "remove": []})).await?.into_result()?;
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "near", "contract": pyth.id(), "price_id": NEAR_ID, "backup": common::backup(pyth.id(), NEAR_ID)},
        {"name": "usdc", "contract": pyth.id(), "price_id": USDC_ID, "backup": common::backup(pyth.id(), USDC_ID)}]}})).await?.into_result()?;
    let node = json!({"input": wnear.id(), "when": {"when": "now"}, "action": {"label": "Swap at market", "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, usdc.id())}], "out": usdc.id(), "min_out_per_in": null, "price": null,
        "market": {"sell": "near", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "m", "nodes": [node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let rate = |r: u128| {
        let (a, d, i, o) = (w.alice.clone(), dex.clone(), wnear.id().clone(), usdc.id().clone());
        async move { a.call(d.id(), "set_rate").args_json(json!({"token_in": i, "token_out": o, "rate": r.to_string()})).transact().await?.into_result()?; anyhow::Ok(()) }
    };
    let bal = |who: &near_workspaces::AccountId| {
        let (u, who) = (usdc.clone(), who.clone());
        async move { anyhow::Ok(u.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse::<u128>()?) }
    };
    // A pool pushed 10% off the market (1.8 USDC per NEAR, the market says 2.00, floor 1.98): refused, nothing lost.
    rate(18 * E24 / 10).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().join("").contains("market_floor") && r.logs().join("").contains("step_failed"), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(bal(&w.vault).await?, 0);
    // A fair pool (2.0): swapped.
    rate(2 * E24).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    println!("GAS relay step with a market floor: {:.1} Tgas", tgas(&r));
    assert!(r.logs().join("").contains(r#""event":"step""#), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(bal(&w.vault).await?, 20 * ONE);
    // No price, no trade: a stale NEAR price refuses the step before anything moves.
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    set(NEAR_ID, 200_000_000, 120).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(fails_with(&r, "no price fresher"), "{:?}", r.failures());
    assert!(w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["busy"].is_null(), "not left busy");
    Ok(())
}
