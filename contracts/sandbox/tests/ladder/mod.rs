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
    /// NVDA per ETH x 1e6: ETH's oracle price follows NVDA's at this ratio (a fork test moves it
    /// to the pool's mid after its own trades: the arbitrage a real chain would do).
    pub ratio_e6: std::cell::Cell<u128>,
}

pub fn tok(addr: &str) -> String {
    format!("evm:4663:{addr}")
}

/// Pyth's ETH/USD id (the ladder's market floors read ETH and NVDA).
pub const ETH_FEED: &str = "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace";
/// NVDA per ETH, x 1e6, in the plain sandbox world: ETH $2,400 at NVDA $180.
pub const RATIO_E6: u128 = 13_333_333;

/// The vault's oracle for the ladder: ETH and NVDA, each confirmed by a second publisher
/// (mock_pyth's backup id; on testnet NVDA's is the NVDA/USDG Uniswap v3 pool's 30-min TWAP).
pub fn ladder_feeds(pyth: &impl std::fmt::Display) -> Value {
    json!([
        {"name": "eth", "contract": pyth.to_string(), "price_id": ETH_FEED, "backup": crate::common::backup(pyth, ETH_FEED)},
        {"name": "nvda", "contract": pyth.to_string(), "price_id": NVDA_FEED, "backup": crate::common::backup(pyth, NVDA_FEED)},
    ])
}

/// A relay vault on Robinhood Chain (mock signer) with the node library's templates and ETH and NVDA prices.
pub async fn world() -> anyhow::Result<World> {
    world_at(RATIO_E6).await
}

/// The same, with ETH priced at `ratio_e6` / 1e6 NVDA (a fork's pool ratio, so its floors can fill).
pub async fn world_at(ratio_e6: u128) -> anyhow::Result<World> {
    let RelayWorld { worker, brain, alice, keeper, signer, vault, .. } = relay_world().await?;
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    for t in crate::common::templates("robinhood") {
        x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": ladder_feeds(pyth.id())}})).await?.into_result()?;
    let w = World { worker, brain, alice, keeper, signer, pyth, vault, ratio_e6: std::cell::Cell::new(ratio_e6) };
    nvda_price(&w, 180, 0).await?;
    Ok(w)
}

/// ETH's price (x 1e8) when NVDA is at `usd`: the pair moves together, at the world's ratio.
pub fn eth_e8(w: &World, usd: i64) -> i64 {
    (usd as u128 * 100_000_000 * w.ratio_e6.get() / 1_000_000) as i64
}

/// The brain's market minimum (oracle::market_min) for prices x 1e8 and same-decimal tokens:
/// amount x p_in x (1 - slippage) / p_out, rounded down. Exact: the 1e10 to 1e18 cancels.
pub fn market_floor(amount: u128, p_in_e8: i64, p_out_e8: i64, slippage_bps: u128) -> u128 {
    amount * p_in_e8 as u128 * (10_000 - slippage_bps) / (p_out_e8 as u128 * 10_000)
}

/// The pair token's share-floor for an ETH zap (oracle::ShareValue, then market_min), to ~1e-12.
pub fn zap_floor(eth_in: u128, eth_e8: i64, nvda_e8: i64, k: u128, slippage_bps: u128) -> f64 {
    let vps = k as f64 / 1e18 * ((eth_e8 as f64) * (nvda_e8 as f64)).sqrt();
    eth_in as f64 * eth_e8 as f64 / vps * (10_000 - slippage_bps) as f64 / 10_000.0
}

/// Chainlink on Robinhood Chain: RHNVDA/USD and AMZN/USD (8 decimals; the Smart LP's own feed).
pub const RHNVDA_FEED: &str = "0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15";
pub const AMZN_CHAINLINK: &str = "0xd5a1508ced74c084ebf3cbe853e2c968fb2a651c";
/// The audited Smart LP vault (StonkBrokers registry index 3, sdAMZN-FR-USDG: 23,718 bytes, library linked).
pub const SMART_LP: &str = "0x68199040683ac90e7c0f47e7b81a8e9edbb61f2f";

/// A Chainlink feed's answer on the fork (x 1e8).
pub fn chainlink_e8(fork: &Fork, proxy: &str) -> i64 {
    let out = fork.cast(&["call", proxy, "latestRoundData()(uint80,int256,uint256,uint256,uint80)"]);
    num(out.lines().nth(1).unwrap()) as i64
}

/// The pair token's k as its owner pins it from the chain: 1e18 x 2 x positionLiquidity() / totalSupply().
pub fn fork_k_pair(fork: &Fork) -> u128 {
    let l = num(&fork.cast(&["call", ETHNVDA, "positionLiquidity()(uint128)"]));
    let s = num(&fork.cast(&["call", ETHNVDA, "totalSupply()(uint256)"]));
    2 * l * (E18 / 1_000_000) / (s / 1_000_000)
}

/// The Smart LP's k from its own total value (USDG, 6 decimals) over its supply, at AMZN `amzn_e8`:
/// value per whole share / sqrt(P_amzn x $1), x 1e18.
pub fn fork_k_smartlp(fork: &Fork, amzn_e8: i64) -> u128 {
    let tv = num(&fork.cast(&["call", SMART_LP, "totalValueQuote()(uint256)"])) as f64 / 1e6;
    let s = num(&fork.cast(&["call", SMART_LP, "totalSupply()(uint256)"])) as f64 / 1e18;
    (tv / s / (amzn_e8 as f64 / 1e8).sqrt() * 1e18) as u128
}

/// A library template with the owner's freshly pinned k in its share leaf.
pub fn with_k(t: &Value, k: u128) -> Value {
    let s = t.to_string();
    let at = s.find(r#""k":""#).expect("a share leaf") + 5;
    let end = at + s[at..].find('"').unwrap();
    serde_json::from_str(&format!("{}{k}{}", &s[..at], &s[end..])).unwrap()
}

/// The k the node library pins for a template's share leaf.
pub fn pinned_k(template: &str) -> u128 {
    let t = crate::common::templates("robinhood").into_iter().find(|t| t["name"] == template).unwrap();
    let s = t.to_string();
    let at = s.find(r#""k":""#).unwrap() + 5;
    s[at..at + s[at..].find('"').unwrap()].parse().unwrap()
}

/// The vault's own address on Robinhood Chain (the brain derives it from the vault's MPC path).
pub async fn evm_address(w: &World) -> anyhow::Result<String> {
    Ok(view(&w.brain, &w.vault, "get_evm", Value::Null).await?["address"].as_str().unwrap().to_string())
}

/// A request to the vault through its `x` door (the brain decides).
pub async fn req(w: &World, who: &Account, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    x(who, &w.vault, method, args).await
}

/// NVDA at `usd` dollars and ETH at the world's ratio (Pyth: x 1e-8, confidence 0.05%,
/// each backup agreeing), published `age_s` ago.
pub async fn nvda_price(w: &World, usd: i64, age_s: i64) -> anyhow::Result<()> {
    set_pair(&w.alice, &w.pyth, usd, w.ratio_e6.get(), age_s).await
}

/// NVDA at `usd` and ETH at `ratio_e6` / 1e6 NVDA on mock_pyth, both with agreeing backups.
pub async fn set_pair(who: &Account, pyth: &Contract, usd: i64, ratio_e6: u128, age_s: i64) -> anyhow::Result<()> {
    let eth = (usd as u128 * 100_000_000 * ratio_e6 / 1_000_000) as i64;
    for (id, p) in [(NVDA_FEED, usd * 100_000_000), (ETH_FEED, eth)] {
        who.call(pyth.id(), "set_price").args_json(json!({"price_id": id, "price": p.to_string(), "conf": (p / 2000).to_string(),
            "expo": -8, "backup": p.to_string(), "age_s": age_s})).transact().await?.into_result()?;
    }
    Ok(())
}

/// NVDA per ETH x 1e6 at the fork's ETH/NVDA v4 pool's mid price, fee taken out: quote a small
/// amount both ways, out/in = mid x (1 - fee) and (1/mid) x (1 - fee), so mid = sqrt(ratio of the two).
pub fn pool_ratio_e6(fork: &Fork) -> u128 {
    let fwd = v4_quote(fork, true, E18 / 1000) as f64 / (E18 / 1000) as f64;
    let back = v4_quote(fork, false, E18 / 100) as f64 / (E18 / 100) as f64;
    ((fwd / back).sqrt() * 1e6) as u128
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
