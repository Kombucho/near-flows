//! Real protocol code (Rhea DEX, Rhea lending, LiNEAR, wNEAR, as deployed on
//! testnet, fetched by scripts/fetch-protocols.sh) around a RELAY vault, every
//! request sent with exactly the 300 Tgas a mainnet transaction may carry.
//! Shared by protocols.rs and gas_mainnet.rs. Build our wasm first: make build
#![allow(dead_code)]

use crate::relayw::{relay_world, RelayWorld};
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract, Worker};
use near_workspaces::network::Sandbox;
use serde_json::{json, Value};

pub const ONE: u128 = 1_000_000_000_000_000_000; // 1 USDC in the mock (18 decimals)
pub const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
pub const SCALE: u128 = NEAR; // prices are scaled 1e24

pub fn ours(name: &str) -> Vec<u8> {
    crate::common::wasm(name)
}

pub fn theirs(id: &str) -> Vec<u8> {
    crate::common::protocol(id)
}

/// Mainnet's cap. FULL_GAS=1 gives 1,000 Tgas instead (to measure what a refused node would burn).
pub fn tx_gas() -> Gas {
    Gas::from_tgas(if std::env::var("FULL_GAS").is_ok() { 1_000 } else { 300 })
}

pub struct World {
    pub worker: Worker<Sandbox>,
    pub lp: Account,
    pub alice: Account,
    pub keeper: Account,
    pub brain: Contract,
    pub wrap: Contract,
    pub usdc: Contract,
    pub rhea: Contract,
    pub linear: Contract,
    pub vault: AccountId,
    pub pool: u64,
}

pub async fn register(payer: &Account, token: &AccountId, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token, "storage_deposit").args_json(json!({"account_id": who, "registration_only": true}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

pub async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

/// A request to the relay vault through `x`, with mainnet's 300 Tgas.
pub async fn x(who: &Account, vault: &AccountId, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(json!({ method: args }).to_string().into_bytes()).gas(tx_gas()).transact().await?)
}

/// Real wNEAR, Rhea and LiNEAR; a pool of 1,000 wNEAR : 2,000 USDC (1 NEAR = 2 USDC);
/// Alice's relay vault trusts all of them.
pub async fn setup() -> anyhow::Result<World> {
    let RelayWorld { worker, brain, alice, keeper, vault, .. } = relay_world().await?;
    let lp = worker.root_account()?;
    let r = crate::rhea::deploy(&worker).await?;
    let usdc = worker.dev_deploy(&ours("mock_ft")).await?;
    usdc.call("new").args_json(json!({"owner_id": lp.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
        "name": "Mock USDC", "symbol": "USDC", "decimals": 18, "call_fee_bps": 0})).transact().await?.into_result()?;
    let linear = worker.dev_deploy(&theirs("linear-protocol.testnet")).await?;
    linear.call("new").args_json(json!({"owner_id": linear.id()})).max_gas().transact().await?.into_result()?;
    // The liquidity provider opens a wNEAR/USDC pool on Rhea.
    let pool = r.pool(&lp, usdc.id(), 1_000 * NEAR, 2_000 * ONE).await?;
    let crate::rhea::Rhea { wrap, dex: rhea } = r;

    for t in [wrap.id(), usdc.id()] {
        register(&lp, t, &vault).await?;
    }
    x(&alice, &vault, "trust", json!({"add": [
        {"contract": wrap.id(), "methods": ["near_deposit"]}, {"contract": usdc.id()},
        {"contract": rhea.id(), "methods": ["get_return"]}, {"contract": linear.id(), "methods": ["deposit_and_stake"]}], "remove": []})).await?.into_result()?;
    Ok(World { worker, lp, alice, keeper, brain, wrap, usdc, rhea, linear, vault, pool })
}

pub fn wrap_near(w: &World) -> Value {
    json!({"label": "Wrap NEAR", "calls": [{"call": "fn", "contract": w.wrap.id(), "method": "near_deposit", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": 10}], "out": w.wrap.id(), "min_out_per_in": null, "price": null})
}

/// A Rhea v1 instant swap, exactly the msg the Rhea SDK builds (the node library's "Swap on Rhea").
pub fn rhea_swap(w: &World, from: &Contract, to: &Contract, min_out_per_in: Option<u128>) -> Value {
    let msg = crate::rhea::Rhea::swap_msg(w.pool, from.id(), to.id());
    json!({"label": "Swap on Rhea", "calls": [{"call": "ft", "receiver": w.rhea.id(), "msg": msg, "tgas": library_tgas("rhea_swap")[0]}],
           "out": to.id(), "min_out_per_in": min_out_per_in.map(|m| m.to_string()),
           "price": {"contract": w.rhea.id(), "method": "get_return",
                     "args": json!({"pool_id": w.pool, "token_in": from.id(), "amount_in": NEAR.to_string(), "token_out": to.id()}).to_string(),
                     "per": NEAR.to_string()}})
}

pub fn linear_stake(w: &World) -> Value {
    json!({"label": "Stake with LiNEAR", "calls": [{"call": "fn", "contract": w.linear.id(), "method": "deposit_and_stake", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": library_tgas("linear_stake")[0]}], "out": w.linear.id(), "min_out_per_in": null, "price": null})
}

/// The gas the node library (nodes/library.json) gives each call of node `id`: tests use the published numbers.
pub fn library_tgas(id: &str) -> Vec<u64> {
    let lib: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../../nodes/library.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    let node = lib["nodes"].as_array().unwrap().iter().find(|n| n["id"] == id).unwrap_or_else(|| panic!("no library node {id}"));
    node["recipe"]["calls"].as_array().unwrap().iter().map(|c| c["tgas"].as_u64().unwrap()).collect()
}

pub async fn save(w: &World, nodes: Value) -> anyhow::Result<u32> {
    Ok(x(&w.alice, &w.vault, "save_flow", json!({"name": "real", "nodes": nodes})).await?.into_result()?.json()?)
}

pub async fn fund(w: &World, flow: u32, node: u16, amount: u128) -> anyhow::Result<()> {
    x(&w.alice, &w.vault, "fund", json!({"flow_id": flow, "node": node, "amount": amount.to_string()})).await?.into_result()?;
    Ok(())
}

/// One step, as `who` (the keeper, or the owner for a manual node), printed for the gas table.
pub async fn step_as(w: &World, who: &Account, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    let r = x(who, &w.vault, "step", json!({"flow_id": flow, "node": node})).await?;
    println!("GAS step: {:.1} Tgas burnt over {} receipts", r.total_gas_burnt.as_gas() as f64 / 1e12, r.receipt_outcomes().len());
    for o in r.receipt_outcomes() {
        if std::env::var("GAS_RECEIPTS").is_ok() {
            println!("    {:>5.1} Tg  {}", o.gas_burnt.as_gas() as f64 / 1e12, o.executor_id);
        }
        if o.is_failure() {
            println!("  FAIL {} {:?}", o.executor_id, o.clone().into_result().err());
        }
        for l in &o.logs {
            if l.contains("flows") {
                println!("  {l}");
            }
        }
    }
    Ok(r)
}

pub async fn step(w: &World, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    step_as(w, &w.keeper, flow, node).await
}

pub fn stepped(r: &ExecutionFinalResult) -> bool {
    r.logs().iter().any(|l| l.contains(r#""event":"step""#))
}

/// Real Rhea lending (its testnet build), configured like testnet: fixed default
/// prices (wNEAR $9.60, USDC $1.00), so no oracle is needed in a local node.
/// The liquidity provider supplies 5,000 USDC for others to borrow.
pub async fn lending(w: &World) -> anyhow::Result<Contract> {
    let lend = w.worker.dev_deploy(&theirs("contract.1689937928.burrow.testnet")).await?;
    let booster = w.worker.dev_deploy(&ours("mock_ft")).await?;
    let r = lend.call("new").args_json(json!({"config": {
        "oracle_account_id": "oracle.test.near", "pyth_oracle_account_id": "pyth.test.near", "ref_exchange_id": w.rhea.id(),
        "owner_id": w.lp.id(), "booster_token_id": booster.id(), "booster_decimals": 18, "max_num_assets": 10,
        "maximum_recency_duration_sec": 90, "maximum_staleness_duration_sec": 15, "lp_tokens_info_valid_duration_sec": 600,
        "pyth_price_valid_duration_sec": 60, "minimum_staking_duration_sec": 2678400, "maximum_staking_duration_sec": 31536000,
        "x_booster_multiplier_at_maximum_staking_duration": 40000, "force_closing_enabled": true,
        "enable_price_oracle": false, "enable_pyth_oracle": true}})).max_gas().transact().await?;
    r.into_result()?;
    let asset = |volatility: u32| json!({"reserve_ratio": 2500, "prot_ratio": 0, "target_utilization": 8000,
        "target_utilization_rate": "1000000000003593629036885046", "max_utilization_rate": "1000000000039724853136740579",
        "holding_position_fee_rate": "1000000000000000000000000000", "volatility_ratio": volatility, "extra_decimals": 0,
        "can_deposit": true, "can_withdraw": true, "can_use_as_collateral": true, "can_borrow": true, "net_tvl_multiplier": 10000});
    for (t, vol, decimals, mult) in [(w.wrap.id(), 6000, 24, "960"), (w.usdc.id(), 9500, 18, "10000")] {
        w.lp.call(lend.id(), "add_asset").args_json(json!({"token_id": t, "asset_config": asset(vol)}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
        let price_decimals = if *t == *w.wrap.id() { 26 } else { 22 };
        w.lp.call(lend.id(), "add_token_pyth_info").args_json(json!({"token_id": t, "token_pyth_info": {
            "decimals": decimals, "fraction_digits": 4, "price_identifier": "00".repeat(32), "extra_call": null,
            "default_price": {"multiplier": mult, "decimals": price_decimals}}}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
        register(&w.lp, t, lend.id()).await?;
    }
    for who in [w.lp.id(), &w.vault] {
        w.lp.call(lend.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(100))
            .transact().await?.into_result()?;
    }
    w.lp.call(w.usdc.id(), "ft_transfer_call").args_json(json!({"receiver_id": lend.id(), "amount": (5_000 * ONE).to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": lend.id(), "methods": ["execute_with_pyth"]}], "remove": []})).await?.into_result()?;
    Ok(lend)
}

/// The node library's "Borrow on Rhea": supply wNEAR as collateral and borrow USDC against it.
/// `usdc_per_wnear` sets how much to borrow per wNEAR supplied ({scaled}).
pub fn supply_and_borrow(w: &World, lend: &Contract, usdc_per_wnear: u128) -> Value {
    let g = library_tgas("rhea_borrow");
    let supply = format!(r#"{{"Execute":{{"actions":[{{"IncreaseCollateral":{{"token_id":"{}","amount":null,"max_amount":null}}}}]}}}}"#, w.wrap.id());
    let borrow = format!(r#"{{"actions":[{{"Borrow":{{"token_id":"{u}","amount":"{{scaled}}","max_amount":null}}}},{{"Withdraw":{{"token_id":"{u}","amount":null,"max_amount":"{{scaled}}"}}}}]}}"#, u = w.usdc.id());
    json!({"label": "Borrow USDC on Rhea", "calls": [
            {"call": "ft", "receiver": lend.id(), "msg": supply, "tgas": g[0]},
            {"call": "fn", "contract": lend.id(), "method": "execute_with_pyth", "args": borrow, "deposit": {"kind": "one_yocto"}, "tgas": g[1]}],
           "out": w.usdc.id(), "min_out_per_in": null, "price": null, "scale_per_in": usdc_per_wnear.to_string() /* per 1 wNEAR = per 1e24 = the scale */})
}

/// The node library's "Repay on Rhea": repay USDC and take wNEAR collateral back;
/// `wnear_per_usdc` (scaled 1e24) sets how much collateral to release per USDC repaid.
pub fn repay_and_release(w: &World, lend: &Contract, wnear_per_usdc: u128) -> Value {
    let g = library_tgas("rhea_repay");
    // The testnet build predates the "OnlyRepay" shortcut: use Execute { Repay }.
    let repay = format!(r#"{{"Execute":{{"actions":[{{"Repay":{{"token_id":"{}","amount":null,"max_amount":null}}}}]}}}}"#, w.usdc.id());
    let release = format!(r#"{{"actions":[{{"DecreaseCollateral":{{"token_id":"{t}","amount":"{{scaled}}","max_amount":null}}}},{{"Withdraw":{{"token_id":"{t}","amount":null,"max_amount":"{{scaled}}"}}}}]}}"#, t = w.wrap.id());
    json!({"label": "Repay on Rhea", "calls": [
            {"call": "ft", "receiver": lend.id(), "msg": repay, "tgas": g[0]},
            {"call": "fn", "contract": lend.id(), "method": "execute_with_pyth", "args": release, "deposit": {"kind": "one_yocto"}, "tgas": g[1]}],
           "out": w.wrap.id(), "min_out_per_in": null, "price": null, "scale_per_in": wnear_per_usdc.to_string()})
}
