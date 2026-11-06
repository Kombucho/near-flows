//! Market floors: a swap's minimum follows the oracle price, so a pool pushed
//! off the market (by a bot, or a thin pool) can't fill the vault at a bad price.
//! The vault reads Pyth-format prices itself (here a stand-in with the real
//! receiver's interface) and refuses stale, uncertain or jumpy prices.

mod common;

use common::wasm;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals)
const SCALE: u128 = 1_000_000_000_000_000_000_000_000;
const NEAR_ID: &str = "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750"; // Pyth NEAR/USD
const USDC_ID: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a"; // Pyth USDC/USD

struct World {
    alice: Account,
    keeper: Account,
    wnear: Contract,
    usdc: Contract,
    dex: Contract,
    pyth: Contract,
    vault: Contract,
}

async fn token(w: &near_workspaces::Worker<near_workspaces::network::Sandbox>, owner: &Account, sym: &str) -> anyhow::Result<Contract> {
    let t = w.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn balance(t: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    Ok(t.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse()?)
}

/// Pyth-style price: `usd_e8` x 1e-8, confidence `conf_bps`, EMA `ema_e8`, published `age_s` ago.
async fn price(w: &World, id: &str, usd_e8: i64, conf_bps: i64, ema_e8: i64, age_s: i64) -> anyhow::Result<()> {
    w.alice.call(w.pyth.id(), "set_price").args_json(json!({"price_id": id, "price": usd_e8.to_string(), "conf": (usd_e8 * conf_bps / 10_000).to_string(),
        "expo": -8, "ema": ema_e8.to_string(), "age_s": age_s})).transact().await?.into_result()?;
    Ok(())
}

async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.dev_create_account().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let (wnear, usdc) = (token(&worker, &root, "wNEAR").await?, token(&worker, &root, "USDC").await?);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": wnear.id()})).transact().await?.into_result()?;
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [alice.id(), dex.id(), vault.id()] {
            root.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
        root.call(t.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (100_000_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    }
    root.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": vault.id(), "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    alice.call(vault.id(), "trust").args_json(json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id()}], "remove": []}))
        .transact().await?.into_result()?;
    // Feeds: NEAR and USDC from Pyth, each confirmed by its EMA within 2%; fresh = under 60 s; confidence under 1%.
    alice.call(vault.id(), "oracle_setup").args_json(json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "near", "contract": pyth.id(), "price_id": NEAR_ID, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": pyth.id(), "price_id": USDC_ID, "backup": {"kind": "ema"}}]}})).transact().await?.into_result()?;
    let w = World { alice, keeper, wnear, usdc, dex, pyth, vault };
    price(&w, NEAR_ID, 200_000_000, 5, 199_000_000, 0).await?; // NEAR $2.00 (EMA $1.99)
    price(&w, USDC_ID, 100_000_000, 5, 100_000_000, 0).await?; // USDC $1.00
    Ok(w)
}

/// A wNEAR -> USDC swap; `market` = follow the oracle with 1% slippage.
fn swap(w: &World, market: bool) -> Value {
    json!({"input": w.wnear.id(), "when": {"when": "now"}, "action": {"label": "Sell NEAR", "out": w.usdc.id(), "min_out_per_in": null, "price": null,
        "calls": [{"call": "ft", "receiver": w.dex.id(), "tgas": 50, "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, w.usdc.id())}],
        "market": if market { json!({"sell": "near", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}) } else { Value::Null }}})
}

async fn flow(w: &World, node: Value, amount: u128) -> anyhow::Result<u32> {
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "sell", "nodes": [node]})).max_gas().transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": amount.to_string()})).max_gas().transact().await?.into_result()?;
    Ok(f)
}

async fn step(w: &World, f: u32) -> anyhow::Result<ExecutionFinalResult> {
    Ok(w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": f, "node": 0})).max_gas().transact().await?)
}

async fn inbox(w: &World, f: u32) -> anyhow::Result<u128> {
    let v: Value = w.vault.view("get_flow").args_json(json!({"flow_id": f})).await?.json()?;
    Ok(v["ledger"]["nodes"][0]["inbox"].as_str().unwrap().parse()?)
}

async fn rate(w: &World, r: u128) -> anyhow::Result<()> {
    w.alice.call(w.dex.id(), "set_rate").args_json(json!({"token_in": w.wnear.id(), "token_out": w.usdc.id(), "rate": r.to_string()})).transact().await?.into_result()?;
    Ok(())
}

#[tokio::test]
async fn a_market_floor_stops_a_pool_pushed_off_the_market() -> anyhow::Result<()> {
    let w = setup().await?;
    // A bot pushed the pool 5% below the market: 1 wNEAR fetches 1.90 USDC while NEAR is $2.00.
    rate(&w, 19 * SCALE / 10).await?;
    let guarded = flow(&w, swap(&w, true), 100 * ONE).await?;
    let naive = flow(&w, swap(&w, false), 100 * ONE).await?;

    // The guarded flow: minimum = 100 x $2 / $1 x 99% = 198 USDC; the pool offers 190: refused, nothing sold.
    let r = step(&w, guarded).await?;
    let logs = r.logs().join("\n");
    assert!(logs.contains(r#""event":"market_floor""#) && logs.contains(r#""min_out_per_in":"1980000000000000000000000""#), "{logs}");
    assert!(logs.contains("step_failed"), "{logs}");
    assert_eq!(inbox(&w, guarded).await?, 100 * ONE, "the 100 wNEAR are back in the flow");
    // The flow without one sells at whatever the pool says: 190 USDC, 10 USDC lost to the bot.
    assert!(step(&w, naive).await?.logs().join("\n").contains(r#""out":"190000000000000000000""#));

    // The pool comes back to the market: the guarded flow sells at 200.
    rate(&w, 2 * SCALE).await?;
    let r = step(&w, guarded).await?;
    assert!(r.logs().join("\n").contains(r#""out":"200000000000000000000""#), "{:?}", r.logs());
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, 390 * ONE);
    Ok(())
}

#[tokio::test]
async fn no_fresh_tight_confirmed_price_no_trade() -> anyhow::Result<()> {
    let w = setup().await?;
    rate(&w, 2 * SCALE).await?;
    let f = flow(&w, swap(&w, true), 100 * ONE).await?;
    let cases = [
        ("stale: published 2 minutes ago", 200_000_000i64, 5i64, 199_000_000i64, 120i64, "no price fresher"),
        ("uncertain: confidence 3%", 200_000_000, 300, 199_000_000, 0, "too uncertain"),
        ("jumpy: spot 10% off its average", 200_000_000, 5, 180_000_000, 0, "disagree"),
        ("no price at all", -1, 0, -1, 0, "bad price"),
    ];
    for (name, p, conf, ema, age, why) in cases {
        price(&w, NEAR_ID, p, conf, ema, age).await?;
        let r = step(&w, f).await?;
        let failures = format!("{:?}", r.failures());
        assert!(failures.contains(why), "{name}: {failures}");
        assert!(!r.logs().join("\n").contains(r#""event":"step""#), "{name}: it ran");
        assert_eq!(inbox(&w, f).await?, 100 * ONE, "{name}: nothing taken");
        assert!(w.vault.view("get_busy").await?.json::<Option<Value>>()?.is_none(), "{name}: the vault isn't left busy");
    }
    // A feed nobody set up is refused too.
    let mut node = swap(&w, true);
    node["action"]["market"]["buy"] = json!("doge");
    let f2 = flow(&w, node, ONE).await?;
    assert!(format!("{:?}", step(&w, f2).await?.failures()).contains("no price feed 'doge'"));
    // Fresh again: it runs.
    price(&w, NEAR_ID, 200_000_000, 5, 199_000_000, 0).await?;
    assert!(step(&w, f).await?.logs().join("\n").contains(r#""event":"step""#));
    Ok(())
}
