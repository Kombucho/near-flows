//! Mainnet caps a transaction at 300 Tgas prepaid; near-workspaces' max_gas is
//! 1,000 Tgas, so every other sandbox test could hide an overrun. Here the
//! heaviest real actions run through relay + brain with EXACTLY 300 Tgas
//! attached, the way a keeper or wallet sends them on mainnet. Each prints a
//! `MAINNET300` row: the action, whether it completed, the gas burnt.
//! Build the wasm first: make build (Intents also needs scripts/fetch-protocols.sh).

mod common;
mod ladder;
mod protocolsw;
mod relayw;
mod rhea;

use common::wasm;
use ed25519_dalek::{Signer, SigningKey};
use ladder::*;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract};
use relayw::*;
use serde_json::{json, Value};
use solguard::testkit::*;
use solguard::*;

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR_FEED: &str = "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750";
const USDC_FEED: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";
const LST_FEED: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const SOL_FEED: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL_MINT: &str = "So11111111111111111111111111111111111111112";
const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
const HOUR_NS: u64 = 3_600 * 1_000_000_000;

/// What mainnet lets a transaction carry.
fn mainnet() -> Gas {
    Gas::from_tgas(300)
}

/// A request to the relay vault with exactly 300 Tgas.
async fn x300(who: &Account, vault: &AccountId, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(json!({ method: args }).to_string().into_bytes()).gas(mainnet()).transact().await?)
}

/// One row of the table, and every receipt that failed.
fn row(what: &str, r: &ExecutionFinalResult, done: bool) {
    println!("MAINNET300 | {what} | {} | {:.1} Tgas burnt, {} receipts", if done { "fits" } else { "DOES NOT FIT" }, tgas(r), r.receipt_outcomes().len());
    for o in r.receipt_outcomes() {
        // GAS_RECEIPTS=1: every receipt's burn, to size the brain's reserves.
        if std::env::var("GAS_RECEIPTS").is_ok() {
            println!("    {:>5.1} Tg  {}", o.gas_burnt.as_gas() as f64 / 1e12, o.executor_id);
        }
        if o.is_failure() {
            println!("    failed in {}: {:?}", o.executor_id, o.clone().into_result().err());
        }
    }
}

fn logged(r: &ExecutionFinalResult, ev: &str) -> bool {
    r.logs().iter().any(|l| l.contains(&format!(r#""event":"{ev}""#)))
}

/// A step completed: the brain logged `step` and no receipt failed.
fn stepped(r: &ExecutionFinalResult) -> bool {
    r.is_success() && logged(r, "step") && r.receipt_outcomes().iter().all(|o| !o.is_failure())
}

struct Market {
    w: RelayWorld,
    minter: Account,
    wnear: Contract,
    usdc: Contract,
    lst: Contract,
    dex: Contract,
    pyth: Contract,
}

async fn ft(w: &RelayWorld, minter: &Account, sym: &str) -> anyhow::Result<Contract> {
    let t = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn give(from: &Account, t: &Contract, to: &AccountId, amount: u128) -> anyhow::Result<()> {
    from.call(t.id(), "ft_transfer").args_json(json!({"receiver_id": to, "amount": amount.to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    Ok(())
}

async fn set_price(w: &RelayWorld, pyth: &Contract, id: &str, usd_e8: i64) -> anyhow::Result<()> {
    w.alice.call(pyth.id(), "set_price").args_json(json!({"price_id": id, "price": usd_e8.to_string(), "conf": "0", "expo": -8, "ema": usd_e8.to_string(), "age_s": 0}))
        .transact().await?.into_result()?;
    Ok(())
}

/// Alice's relay vault with 1,000 wNEAR, 5 NEAR to spare, a dex (wNEAR <-> USDC at 2, NEAR -> LST 1:1),
/// an oracle (NEAR $2, USDC $1, LST $2) and everything trusted.
async fn market() -> anyhow::Result<Market> {
    let w = relay_world().await?;
    let minter = w.worker.dev_create_account().await?;
    let (wnear, usdc, lst) = (ft(&w, &minter, "wNEAR").await?, ft(&w, &minter, "USDC").await?, ft(&w, &minter, "LST").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc, &lst] {
        for who in [dex.id(), &w.vault] {
            minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
        give(&minter, t, dex.id(), 1_000_000 * ONE).await?;
    }
    give(&minter, &wnear, &w.vault, 1_000 * ONE).await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(5)).await?.into_result()?;
    for (a, b, r) in [(&wnear, &usdc, 2 * E24), (&usdc, &wnear, E24 / 2)] {
        w.alice.call(dex.id(), "set_rate").args_json(json!({"token_in": a.id(), "token_out": b.id(), "rate": r.to_string()})).transact().await?.into_result()?;
    }
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    set_price(&w, &pyth, NEAR_FEED, 200_000_000).await?;
    set_price(&w, &pyth, USDC_FEED, 100_000_000).await?;
    set_price(&w, &pyth, LST_FEED, 200_000_000).await?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": lst.id()},
        {"contract": dex.id(), "methods": ["get_return", "deposit_and_stake"]}], "remove": []})).await?.into_result()?;
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "near", "contract": pyth.id(), "price_id": NEAR_FEED, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": pyth.id(), "price_id": USDC_FEED, "backup": {"kind": "ema"}},
        {"name": "lst", "contract": pyth.id(), "price_id": LST_FEED, "backup": {"kind": "ema"}}]}})).await?.into_result()?;
    Ok(Market { w, minter, wnear, usdc, lst, dex, pyth })
}

fn swap_call(m: &Market, tgas: u64) -> Value {
    json!({"call": "ft", "receiver": m.dex.id(), "tgas": tgas, "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, m.usdc.id())})
}

/// wNEAR -> USDC on the dex with a market floor (NEAR/USDC from the oracle, 1% slippage).
fn market_swap(m: &Market, tgas: u64) -> Value {
    json!({"label": "Swap at market", "calls": [swap_call(m, tgas)], "out": m.usdc.id(), "min_out_per_in": null, "price": null,
           "market": {"sell": "near", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}})
}

async fn save_and_fund(m: &Market, node: Value, amount: u128) -> anyhow::Result<u32> {
    let f: u32 = x(&m.w.alice, &m.w.vault, "save_flow", json!({"name": "gas", "nodes": [node]})).await?.json()?;
    x(&m.w.alice, &m.w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": amount.to_string()})).await?.into_result()?;
    Ok(f)
}

#[tokio::test]
async fn a_near_swap_with_a_price_trigger_and_a_market_floor_fits_in_300_tgas() -> anyhow::Result<()> {
    let m = market().await?;
    // Native NEAR in (stake 1:1 for LST), fired by a price read from the dex, floored by the oracle:
    // every read a step can make (trigger price, two feeds, the output balance before and after).
    let node = json!({"input": "near", "when": {"when": "price_above", "price": E24.to_string()},
        "action": {"label": "Stake at market", "calls": [{"call": "fn", "contract": m.dex.id(), "method": "deposit_and_stake", "args": "{}", "deposit": {"kind": "input"}, "tgas": 30}],
            "out": m.lst.id(), "min_out_per_in": null,
            "price": {"contract": m.dex.id(), "method": "get_return", "args": json!({"token_in": m.wnear.id(), "amount_in": ONE.to_string(), "token_out": m.usdc.id()}).to_string(), "per": ONE.to_string()},
            "market": {"sell": "near", "buy": "lst", "dec_in": 24, "dec_out": 18, "slippage_bps": 100}}});
    let f = save_and_fund(&m, node, E24).await?;
    let r = x300(&m.w.keeper, &m.w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row("NEAR stake step: price trigger + market floor + balances", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn a_token_swap_with_a_market_floor_fits_in_300_tgas() -> anyhow::Result<()> {
    let m = market().await?;
    // The node library's "Swap on Rhea" gives its call 120 Tgas.
    let f = save_and_fund(&m, json!({"input": m.wnear.id(), "when": {"when": "now"}, "action": market_swap(&m, 120)}), 10 * ONE).await?;
    let r = x300(&m.w.keeper, &m.w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row("wNEAR->USDC step (120 Tgas call) + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

fn get_return(m: &Market, tgas: u64) -> Value {
    json!({"call": "fn", "contract": m.dex.id(), "method": "get_return", "deposit": {"kind": "none"}, "tgas": tgas,
        "args": json!({"token_in": m.wnear.id(), "amount_in": "{amount}", "token_out": m.usdc.id()}).to_string()})
}

/// A recipe of `calls` (behind a market floor if `floor`), funded and stepped with 300 Tgas.
async fn step_recipe(m: &Market, calls: Vec<Value>, floor: bool) -> anyhow::Result<ExecutionFinalResult> {
    let f = save_and_fund(m, recipe_node(m, calls, floor), 10 * ONE).await?;
    x300(&m.w.keeper, &m.w.vault, "step", json!({"flow_id": f, "node": 0})).await
}

fn recipe_node(m: &Market, calls: Vec<Value>, floor: bool) -> Value {
    let mut action = market_swap(m, 50);
    action["calls"] = json!(calls);
    if !floor {
        action["market"] = Value::Null;
    }
    json!({"input": m.wnear.id(), "when": {"when": "now"}, "action": action})
}

// The budget, from the brain's constants: 300 - 5 (relay x) - 40 (first round) = 255 Tgas
// for the rest. A recipe of k calls takes its calls' gas + 13 per call (8 callback, 5 relay)
// + 25 more after the last (its balance read) + 5. A market floor's 4 oracle views move the
// first read to a round of its own: +79.

#[tokio::test]
async fn two_calls_without_a_floor_fit_up_to_199_tgas_of_calls() -> anyhow::Result<()> {
    let m = market().await?;
    let r = step_recipe(&m, vec![swap_call(&m, 79), get_return(&m, 120)], false).await?;
    let done = stepped(&r);
    row("2-call recipe (79 + 120 Tgas)", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn two_calls_with_a_market_floor_fit_up_to_120_tgas_of_calls() -> anyhow::Result<()> {
    let m = market().await?;
    let r = step_recipe(&m, vec![swap_call(&m, 60), get_return(&m, 60)], true).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row("2-call recipe (60 + 60 Tgas) + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn four_calls_with_a_market_floor_fit_up_to_94_tgas_of_calls() -> anyhow::Result<()> {
    let m = market().await?;
    let r = step_recipe(&m, vec![swap_call(&m, 50), get_return(&m, 14), get_return(&m, 14), get_return(&m, 14)], true).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row("4-call recipe (50 + 3 x 14 Tgas) + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn a_node_too_heavy_for_mainnet_is_refused_when_saved_with_its_gas_need() -> anyhow::Result<()> {
    let m = market().await?;
    for (calls, floor, need) in [
        (vec![swap_call(&m, 60), get_return(&m, 120)], true, 360),
        (vec![swap_call(&m, 80), get_return(&m, 120)], false, 301),
    ] {
        let r = x(&m.w.alice, &m.w.vault, "save_flow", json!({"name": "heavy", "nodes": [recipe_node(&m, calls, floor)]})).await?;
        assert!(fails_with(&r, &format!("node 0 needs {need} Tgas per step")), "{:?}", r.failures());
    }
    Ok(())
}

/// The node library's Borrow and Repay on Rhea, on the real Rhea lending code.
#[tokio::test]
async fn borrow_and_repay_on_rhea_from_the_node_library_fit_in_300_tgas() -> anyhow::Result<()> {
    use protocolsw as p;
    let w = p::setup().await?;
    let lend = p::lending(&w).await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(30)).await?.into_result()?;
    let f = p::save(&w, json!([
        {"input": "near", "action": p::wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": p::supply_and_borrow(&w, &lend, 3 * ONE), "when": {"when": "now"}},
    ])).await?;
    p::fund(&w, f, 0, 10 * p::NEAR).await?;
    let r = p::step(&w, f, 0).await?;
    row("Wrap NEAR (library, 10 Tgas)", &r, p::stepped(&r));
    assert!(p::stepped(&r), "{:?}", r.failures());
    let g = p::library_tgas("rhea_borrow");
    let r = p::step(&w, f, 1).await?;
    row(&format!("Borrow on Rhea (library, {} + {} Tgas)", g[0], g[1]), &r, p::stepped(&r));
    assert!(p::stepped(&r), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(p::balance(&w.usdc, &w.vault).await?, 30 * ONE, "borrowed 30 USDC");
    let g = p::library_tgas("rhea_repay");
    // Release half the collateral (1/6 wNEAR per USDC): Rhea keeps at least 1e6 supply shares
    // of an asset, and this vault is its only wNEAR supplier here.
    let back = p::save(&w, json!([{"input": w.usdc.id(), "action": p::repay_and_release(&w, &lend, p::NEAR / 6 * (p::SCALE / ONE)), "when": {"when": "manual"}}])).await?;
    p::fund(&w, back, 0, 30 * ONE).await?;
    let r = p::step_as(&w, &w.alice, back, 0).await?;
    row(&format!("Repay on Rhea (library, {} + {} Tgas)", g[0], g[1]), &r, p::stepped(&r));
    assert!(p::stepped(&r), "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn a_hidden_flow_step_with_a_market_floor_fits_in_300_tgas() -> anyhow::Result<()> {
    let m = market().await?;
    let node = json!({"input": m.wnear.id(), "when": {"when": "now"}, "action": market_swap(&m, 120)});
    let salt = "aa".repeat(32);
    let print: String = m.w.brain.view("fingerprint_of").args_json(json!({"node": node, "salt": salt})).await?.json()?;
    let f: u32 = x(&m.w.alice, &m.w.vault, "save_hidden_flow", json!({"name": "secret", "inputs": [m.wnear.id()], "fingerprints": [print]})).await?.json()?;
    x(&m.w.alice, &m.w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let r = x300(&m.w.keeper, &m.w.vault, "step_hidden", json!({"flow_id": f, "node": 0, "reveal": node, "salt": salt})).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row("hidden-flow step_hidden (120 Tgas call) + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

/// The owner's key, off chain: signs exactly what the keeper's stealth.ts signs.
struct Owner {
    key: SigningKey,
    nonce: u64,
}

impl Owner {
    fn public(&self) -> String {
        format!("ed25519:{}", bs58::encode(self.key.verifying_key().as_bytes()).into_string())
    }
    fn sign(&mut self, vault: &AccountId, method: &str, args: Value, tgas: u64) -> Value {
        self.nonce += 1;
        let args = args.to_string();
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 + HOUR_NS;
        let text = format!("near-flows stealth v1\nvault: {vault}\nnonce: {}\nexpires_ns: {expires}\ntgas: {tgas}\nmethod: {method}\nargs: {args}", self.nonce);
        let sig = hex::encode(self.key.sign(text.as_bytes()).to_bytes());
        json!({"method": method, "args": args, "nonce": self.nonce.to_string(), "expires_ns": expires.to_string(), "tgas": tgas, "signature": sig})
    }
}

/// The signed gas the keeper's stealth.ts gives a stealth `step` (keep in step with STEP_TGAS there).
const STEALTH_STEP_TGAS: u64 = 80;

#[tokio::test]
async fn a_stealth_exec_step_with_a_market_floor_fits_in_300_tgas() -> anyhow::Result<()> {
    let m = market().await?;
    let factory: AccountId = m.w.vault.as_str().split_once('.').unwrap().1.parse()?;
    let mut owner = Owner { key: SigningKey::from_bytes(&[7; 32]), nonce: 0 };
    m.w.keeper.call(&factory, "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": owner.public(), "keeper": m.w.keeper.id()}))
        .deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let sv: AccountId = format!("k7xq2m.{factory}").parse()?;
    for t in [&m.wnear, &m.usdc] {
        m.minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": sv})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    }
    give(&m.minter, &m.wnear, &sv, 100 * ONE).await?;
    let exec = |s: Value| {
        let (k, sv) = (m.w.keeper.clone(), sv.clone());
        async move { anyhow::Ok(k.call(&sv, "exec").args_json(s).gas(mainnet()).transact().await?) }
    };
    exec(owner.sign(&sv, "trust", json!({"add": [{"contract": m.wnear.id()}, {"contract": m.usdc.id()}, {"contract": m.dex.id()}], "remove": []}), 50)).await?.into_result()?;
    exec(owner.sign(&sv, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "near", "contract": m.pyth.id(), "price_id": NEAR_FEED, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": m.pyth.id(), "price_id": USDC_FEED, "backup": {"kind": "ema"}}]}}), 50)).await?.into_result()?;
    let node = json!({"input": m.wnear.id(), "when": {"when": "manual"}, "action": market_swap(&m, 50)});
    exec(owner.sign(&sv, "save_flow", json!({"name": "s", "nodes": [node]}), 50)).await?.into_result()?;
    exec(owner.sign(&sv, "fund", json!({"flow_id": 0, "node": 0, "amount": (10 * ONE).to_string()}), 50)).await?.into_result()?;
    let r = exec(owner.sign(&sv, "step", json!({"flow_id": 0, "node": 0}), STEALTH_STEP_TGAS)).await?;
    let done = stepped(&r) && logged(&r, "market_floor");
    row(&format!("stealth exec(step), signed {STEALTH_STEP_TGAS} Tgas, 50 Tgas call + market floor"), &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

#[tokio::test]
async fn an_oracle_triggered_remote_evm_step_fits_in_300_tgas() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    for t in common::templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "nvda", "contract": pyth.id(), "price_id": NVDA_FEED, "backup": {"kind": "ema"}}]}})).await?.into_result()?;
    let nvda = |usd: i64| {
        let (a, p) = (w.alice.clone(), pyth.clone());
        async move {
            let p8 = usd * 100_000_000;
            a.call(p.id(), "set_price").args_json(json!({"price_id": NVDA_FEED, "price": p8.to_string(), "conf": (p8 / 2000).to_string(), "expo": -8, "ema": p8.to_string(), "age_s": 0}))
                .transact().await?.into_result()?;
            anyhow::Ok(())
        }
    };
    nvda(180).await?;
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "NVDA ladder", "nodes": ladder()})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": "30000000000000000"})).await?.into_result()?;
    let r = x300(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": evm_args(v4_values(1, true), 0)})).await?;
    let done = r.is_success() && logged(&r, "evm_signed");
    row("step_remote, EVM template (buy)", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": f, "node": 0, "out": "355200000000000000"})).await?.into_result()?;
    nvda(275).await?;
    let r = x300(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 1, "args": evm_args(v4_values(1, false), 1)})).await?;
    let done = r.is_success() && logged(&r, "remote_price") && logged(&r, "evm_signed");
    row("step_remote, oracle-triggered EVM template (rung)", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    let r = x300(&w.keeper, &w.vault, "remote_report", json!({"flow_id": f, "node": 1, "out": "9700000000000000"})).await?;
    row("remote_report", &r, r.is_success());
    assert!(r.is_success(), "{:?}", r.failures());
    Ok(())
}

#[tokio::test]
async fn an_intents_swap_with_a_market_floor_fits_in_300_tgas() -> anyhow::Result<()> {
    let m = market().await?;
    let w = &m.w;
    let verifier = w.worker.dev_deploy(&std::fs::read(format!("{}/protocols/verifier.tidepool-sandbox.testnet.wasm", env!("CARGO_MANIFEST_DIR"))).expect("scripts/fetch-protocols.sh")).await?;
    verifier.call("new").args_json(json!({"config": {"wnear_id": m.wnear.id(), "fees": {"fee": 1, "fee_collector": m.minter.id()}, "roles": {}}})).max_gas().transact().await?.into_result()?;
    m.minter.call(m.wnear.id(), "storage_deposit").args_json(json!({"account_id": verifier.id()})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    let r = x300(&w.alice, &w.vault, "intents_setup", json!({"verifier": verifier.id(), "signer": w.signer.id()})).await?;
    row("intents_setup", &r, r.is_success());
    r.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": verifier.id()}], "remove": []})).await?.into_result()?;
    x(&w.alice, &w.vault, "intents_allow", json!({"pair": {"token_in": m.wnear.id(), "token_out": m.usdc.id(), "max_in": (200 * ONE).to_string(), "floor_out": "1", "floor_per_in": "1",
        "market": {"sell": "near", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}})).await?.into_result()?;
    let r = x300(&w.keeper, &w.vault, "intents_sign_swap", json!({"token_in": m.wnear.id(), "token_out": m.usdc.id(), "amount_in": (100 * ONE).to_string(), "min_out": "1", "valid_secs": 600})).await?;
    let done = r.is_success() && logged(&r, "intents_signed");
    row("intents_sign_swap + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    let signed: Value = r.json()?;
    assert!(signed["payload"]["message"].as_str().unwrap().contains(&format!(r#""nep141:{}":"{}""#, m.usdc.id(), 198 * ONE)), "the market floor (NEAR $2, 1%): {signed}");
    Ok(())
}

#[tokio::test]
async fn solana_signing_fits_in_300_tgas() -> anyhow::Result<()> {
    let w = relay_world().await?;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me = pubkey(w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap());
    // sol_sign_tx: wrap 0.5 SOL into the vault's own wSOL account (the node library's templates).
    for t in ["wsol_wrap", "wsol_sync"] {
        x(&w.alice, &w.vault, "sol_template", json!({"template": common::sol_template(t, &[])})).await?.into_result()?;
    }
    let wsol = ata_of(&me, &pubkey(WSOL_MINT));
    let mut transfer = vec![2u8, 0, 0, 0];
    transfer.extend(500_000_000u64.to_le_bytes());
    let msg = Tx::new(me).ix(pubkey(SYSTEM_PROGRAM), vec![me, wsol], transfer).ix(pubkey(TOKEN_PROGRAM_ID), vec![wsol], vec![17]).bytes();
    let b64 = |b: &[u8]| { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(b) };
    let r = x300(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": b64(&msg)})).await?;
    let done = r.is_success() && logged(&r, "sol_signed");
    row("sol_sign_tx (template: wrap + sync)", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());

    // sol_sign_swap with a market floor: the oracle read, then the message checked again.
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    set_price(&w, &pyth, USDC_FEED, 100_000_000).await?;
    set_price(&w, &pyth, SOL_FEED, 20_000_000_000).await?;
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "sol", "contract": pyth.id(), "price_id": SOL_FEED, "backup": {"kind": "ema"}},
        {"name": "usdc", "contract": pyth.id(), "price_id": USDC_FEED, "backup": {"kind": "ema"}}]}})).await?.into_result()?;
    x(&w.alice, &w.vault, "sol_allow", json!({"pair": {"in_mint": USDC_MINT, "out_mint": WSOL_MINT, "max_in": "50000000", "min_out_per_in_e12": "4000000000000",
        "market": {"sell": "usdc", "buy": "sol", "dec_in": 6, "dec_out": 9, "slippage_bps": 100}}})).await?.into_result()?;
    let r = x300(&w.keeper, &w.vault, "sol_sign_swap", json!({"message": b64(&honest_for(me, pubkey(USDC_MINT), pubkey(WSOL_MINT), 1_000_000, 5_000_000))})).await?;
    let done = r.is_success() && logged(&r, "sol_signed");
    row("sol_sign_swap (Jupiter) + market floor", &r, done);
    assert!(done, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}
