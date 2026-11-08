//! The NVDA take-profit ladder shared by the sandbox test (remote.rs) and the
//! Robinhood Chain fork run (ladder_fork_robinhood.rs): one relay vault, one flow
//! definition. Files that use it also need `mod relayw;` (the world it builds on).
#![allow(dead_code)]

use crate::common::{num, wasm, Fork};
use crate::relayw::{relay_world, view, x, RelayWorld};
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

pub const GWEI: u128 = 1_000_000_000;
pub const E18: u128 = 1_000_000_000_000_000_000;
pub const E24: u128 = 1_000_000_000_000_000_000_000_000;
pub const NVDA: &str = "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec";
pub const ETHNVDA: &str = "0x0d142f0a847838f933114869f6f63bd618bedca8";
/// Pyth's 24/7 NVDA/USD feed.
pub const NVDA_FEED: &str = "a470c4ac46f44b547b2cba52338f311fb642b79375ce5f0cfd5cb5b99227b852";
pub const V4_QUOTER: &str = "0x8dc178efb8111bb0973dd9d722ebeff267c98f94";
pub const ETH: &str = "0x0000000000000000000000000000000000000000";

/// Alice's relay vault (its rules live in `brain`) and the NVDA price feed.
pub struct World {
    pub worker: Worker<Sandbox>,
    pub brain: Contract,
    pub alice: Account,
    pub keeper: Account,
    pub signer: Contract,
    pub pyth: Contract,
    pub vault: AccountId,
}

pub fn tok(addr: &str) -> String {
    format!("evm:4663:{addr}")
}

/// A relay vault on Robinhood Chain (mock signer) with the node library's templates and an NVDA price feed.
pub async fn world() -> anyhow::Result<World> {
    let RelayWorld { worker, brain, alice, keeper, signer, vault, .. } = relay_world().await?;
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    for t in crate::common::templates("robinhood") {
        x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        {"name": "nvda", "contract": pyth.id(), "price_id": NVDA_FEED, "backup": crate::common::single_source()}]}})).await?.into_result()?;
    let w = World { worker, brain, alice, keeper, signer, pyth, vault };
    nvda_price(&w, 180, 0).await?;
    Ok(w)
}

/// The vault's own address on Robinhood Chain (the brain derives it from the vault's MPC path).
pub async fn evm_address(w: &World) -> anyhow::Result<String> {
    Ok(view(&w.brain, &w.vault, "get_evm", Value::Null).await?["address"].as_str().unwrap().to_string())
}

/// A request to the vault through its `x` door (the brain decides).
pub async fn req(w: &World, who: &Account, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    x(who, &w.vault, method, args).await
}

/// NVDA at `usd` dollars (Pyth: x 1e-8, confidence 0.05%, EMA equal), published `age_s` ago.
pub async fn nvda_price(w: &World, usd: i64, age_s: i64) -> anyhow::Result<()> {
    let p = usd * 100_000_000;
    w.alice.call(w.pyth.id(), "set_price").args_json(json!({"price_id": NVDA_FEED, "price": p.to_string(), "conf": (p / 2000).to_string(),
        "expo": -8, "backup": p.to_string(), "age_s": age_s})).transact().await?.into_result()?;
    Ok(())
}

/// The ladder: 0 buys NVDA with ETH and splits it in three; 1-3 sell a third each when
/// NVDA reaches 1.5x / 2x / 3x of $180; every sale's ETH goes to 4, which zaps it into
/// StonkBrokers' ETHNVDA pair token (the basket).
pub fn ladder() -> Vec<Value> {
    let eth = tok("eth");
    let v4 = |template: &str| json!({"chain": "evm", "template": template, "amount_leaf": 8, "min_leaf": 9, "deadline_leaf": 15});
    let node = |input: &str, label: &str, call: Value, out: &str, cap: u128, when: Value, next: Value| json!({"input": input, "when": when, "next": next,
        "action": {"label": label, "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": call, "out": out, "max_out_per_in": cap.to_string(), "price": {"feed": "nvda"}}}});
    let rung = |x10: u128| node(&tok(NVDA), &format!("Sell a third at {}x", x10 as f64 / 10.0), v4("v4_swap_nvda_eth"), &eth, 2 * E24 / 10,
        json!({"when": "price_above", "price": (180 * x10 / 10 * E24).to_string()}), json!([{"to": 4, "bps": 10_000}]));
    vec![
        // At most 20 NVDA per ETH may be reported (the pool gives ~11.8), 0.2 ETH per NVDA (~0.08), 3 shares per ETH (~1.7).
        node(&eth, "Buy NVDA", v4("v4_swap_eth_nvda"), &tok(NVDA), 20 * E24, json!({"when": "now"}),
            json!([{"to": 1, "bps": 3_333}, {"to": 2, "bps": 3_333}, {"to": 3, "bps": 3_333}])),
        rung(15),
        rung(20),
        rung(30),
        node(&eth, "Into the ETHNVDA basket", json!({"chain": "evm", "template": "pairtoken_ethnvda_zap_eth", "amount_leaf": 1, "min_leaf": 2}),
            &tok(ETHNVDA), 3 * E24, json!({"when": "now"}), json!([])),
    ]
}

/// Keeper values for a v4 swap: 17 leaves (16 without the ETH value); the vault fills 8 (amount) and 15 (deadline).
pub fn v4_values(min_out: u128, eth_in: bool) -> Vec<String> {
    let mut v = vec![0u128; if eth_in { 17 } else { 16 }];
    v[8] = 999 * E18; // the keeper "asks" for 999: the vault uses the node's input instead
    v[9] = min_out;
    v.iter().map(|x| x.to_string()).collect()
}

pub fn evm_args(values: Vec<String>, nonce: u64) -> Value {
    json!({"chain": "evm", "values": values, "nonce": nonce, "max_fee": (GWEI / 10).to_string(), "max_tip": "0", "gas": 1_000_000u64})
}

pub async fn step(w: &World, who: &Account, flow: u32, node: u16, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    x(who, &w.vault, "step_remote", json!({"flow_id": flow, "node": node, "args": args})).await
}

pub async fn report(w: &World, who: &Account, flow: u32, node: u16, out: Option<u128>) -> anyhow::Result<ExecutionFinalResult> {
    x(who, &w.vault, "remote_report", json!({"flow_id": flow, "node": node, "out": out.map(|o| o.to_string())})).await
}

pub async fn ledger(w: &World, flow: u32) -> anyhow::Result<Value> {
    Ok(view(&w.brain, &w.vault, "get_flow", json!({"flow_id": flow})).await?["ledger"].clone())
}

pub fn inbox(l: &Value, i: usize) -> u128 {
    l["nodes"][i]["inbox"].as_str().unwrap().parse().unwrap()
}

pub fn event(r: &ExecutionFinalResult, ev: &str) -> Value {
    let log = r.logs().iter().find(|l| l.contains(&format!(r#""event":"{ev}""#))).unwrap_or_else(|| panic!("no {ev}: {:?} {:?}", r.logs(), r.failures())).to_string();
    serde_json::from_str::<Value>(log.trim_start_matches("EVENT_JSON:")).unwrap()["data"][0].clone()
}


/// Broadcast on a fork; returns (status, gas paid in wei).
pub fn send(fork: &Fork, raw: &str) -> (u64, u128) {
    let hash: String = serde_json::from_str(&fork.cast(&["rpc", "eth_sendRawTransaction", raw])).unwrap();
    let r: Value = serde_json::from_str(&fork.cast(&["receipt", &hash, "--json"])).unwrap();
    let hex = |k: &str| u128::from_str_radix(r[k].as_str().unwrap().trim_start_matches("0x"), 16).unwrap();
    (hex("status") as u64, hex("gasUsed") * hex("effectiveGasPrice"))
}

/// What the ETH/NVDA Uniswap v4 pool on the fork pays for `amount` (ETH in if `zero_for_one`).
pub fn v4_quote(fork: &Fork, zero_for_one: bool, amount: u128) -> u128 {
    num(&fork.cast(&["call", V4_QUOTER, "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))(uint256,uint256)",
        &format!("(({ETH},{NVDA},10000,200,{ETH}),{zero_for_one},{amount},0x)")]))
}
