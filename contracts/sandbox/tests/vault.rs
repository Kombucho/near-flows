//! Vault integration tests: a user's own relay vault (the body) and the shared
//! brain (its rules) running flows in a local NEAR node, against a test exchange
//! and staking pool (mock_dex) with fixed rates. Every request goes through the
//! vault's `x` door as `{"<method>": args}`; every view is the brain's, about
//! this vault. (These were the full vault's tests; same behaviours, relay + brain.)
//! Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::{Gas, NearToken};
use near_workspaces::{Account, AccountId, Contract, Worker};
use relayw::{fails_with, relay_world, view, RelayWorld};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals)
const SCALE: u128 = 1_000_000_000_000_000_000_000_000; // rates are scaled 1e24
const NEAR: u128 = SCALE; // 1 NEAR in yocto

struct World {
    worker: Worker<Sandbox>,
    brain: Contract,
    alice: Account,
    keeper: Account,
    mallory: Account,
    wnear: Contract,
    usdc: Contract,
    lst: Contract,
    dex: Contract,
    vault: AccountId,
}

async fn token(worker: &Worker<Sandbox>, owner: &Account, symbol: &str) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new")
        .args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
                          "name": format!("Mock {symbol}"), "symbol": symbol, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn register(payer: &Account, token: &Contract, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token.id(), "storage_deposit").args_json(json!({"account_id": who}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

async fn give(from: &Account, token: &Contract, to: &AccountId, amount: u128) -> anyhow::Result<()> {
    from.call(token.id(), "ft_transfer").args_json(json!({"receiver_id": to, "amount": amount.to_string()}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    Ok(())
}

async fn rate(w: &World, from: &Contract, to: &Contract, r: u128) -> anyhow::Result<()> {
    w.alice.call(w.dex.id(), "set_rate").args_json(json!({"token_in": from.id(), "token_out": to.id(), "rate": r.to_string()}))
        .transact().await?.into_result()?;
    Ok(())
}

/// A request to the vault, as `who`.
async fn x(w: &World, who: &Account, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    relayw::x(who, &w.vault, method, args).await
}

/// Alice owns a relay vault holding 1,000 wNEAR; the dex holds reserves of everything.
async fn setup() -> anyhow::Result<World> {
    let RelayWorld { worker, brain, alice, keeper, vault, .. } = relay_world().await?;
    let (root, mallory) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let (wnear, usdc, lst) = (token(&worker, &root, "wNEAR").await?, token(&worker, &root, "USDC").await?, token(&worker, &root, "LST").await?);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc, &lst] {
        for who in [alice.id(), dex.id(), &vault, mallory.id()] {
            register(&root, t, who).await?;
        }
        give(&root, t, dex.id(), 100_000_000 * ONE).await?;
    }
    give(&root, &wnear, alice.id(), 10_000 * ONE).await?;
    give(&alice, &wnear, &vault, 1_000 * ONE).await?;
    relayw::x(&alice, &vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": lst.id()},
        {"contract": dex.id(), "methods": ["deposit_and_stake", "get_return"]}], "remove": []})).await?.into_result()?;
    let w = World { worker, brain, alice, keeper, mallory, wnear, usdc, lst, dex, vault };
    rate(&w, &w.wnear, &w.usdc, 2 * SCALE).await?; // 1 wNEAR = 2 USDC
    rate(&w, &w.usdc, &w.wnear, SCALE / 2).await?;
    Ok(w)
}

fn swap(w: &World, to: &Contract, min_out_per_in: Option<u128>) -> Value {
    json!({"label": "Swap", "calls": [{"call": "ft", "receiver": w.dex.id(), "tgas": 50,
            "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, to.id())}],
           "out": to.id(), "min_out_per_in": min_out_per_in.map(|m| m.to_string()), "price": null})
}

async fn save(w: &World, nodes: Value) -> anyhow::Result<u32> {
    Ok(x(w, &w.alice, "save_flow", json!({"name": "test", "nodes": nodes})).await?.into_result()?.json()?)
}

async fn fund(w: &World, flow: u32, node: u16, amount: u128) -> anyhow::Result<ExecutionFinalResult> {
    x(w, &w.alice, "fund", json!({"flow_id": flow, "node": node, "amount": amount.to_string()})).await
}

async fn set_limit(w: &World, max: Option<u128>, window_ns: u64) -> anyhow::Result<ExecutionFinalResult> {
    x(w, &w.alice, "set_limit", json!({"token": w.wnear.id(), "max": max.map(|m| m.to_string()), "window_ns": window_ns})).await
}

async fn step(w: &World, who: &Account, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    let r = x(w, who, "step", json!({"flow_id": flow, "node": node})).await?;
    println!("GAS step: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    for o in r.receipt_outcomes() {
        if o.is_failure() || !o.logs.is_empty() {
            println!("  {} {:?} {:?}", o.executor_id, o.logs, o.clone().into_result().err());
        }
    }
    Ok(r)
}

async fn flow_state(w: &World, flow: u32) -> anyhow::Result<Value> {
    view(&w.brain, &w.vault, "get_flow", json!({"flow_id": flow})).await
}

async fn busy(w: &World) -> anyhow::Result<Value> {
    Ok(view(&w.brain, &w.vault, "get_vault", Value::Null).await?["busy"].clone())
}

fn logs(r: &ExecutionFinalResult) -> String {
    r.logs().join("\n")
}

#[tokio::test]
async fn a_swap_splits_its_output_down_the_flow() -> anyhow::Result<()> {
    let w = setup().await?;
    // 100 wNEAR -> USDC; 60% of the USDC goes to a second swap back to wNEAR, 40% stays free.
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(19 * SCALE / 10)), "when": {"when": "now"}, "next": [{"to": 1, "bps": 6000}]});
    let n1 = json!({"input": w.usdc.id(), "action": swap(&w, &w.wnear, None), "when": {"when": "now"}});
    let f = save(&w, json!([n0, n1])).await?;
    fund(&w, f, 0, 100 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(logs(&r).contains(r#""out":"200000000000000000000""#), "{}", logs(&r));
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (120 * ONE).to_string(), "60% of 200 USDC routed on");
    assert_eq!(balance(&w.usdc, &w.vault).await?, 200 * ONE);
    let alloc = view(&w.brain, &w.vault, "get_allocated", json!({"token": w.usdc.id()})).await?;
    assert_eq!(alloc, json!((120 * ONE).to_string()), "the other 80 USDC is free");
    // Second hop: 120 USDC -> 60 wNEAR, nothing routed, all free.
    assert!(step(&w, &w.keeper, f, 1).await?.is_success());
    assert_eq!(balance(&w.wnear, &w.vault).await?, 960 * ONE);
    // Alice withdraws free money; she can't withdraw what a flow holds.
    fund(&w, f, 0, 900 * ONE).await?.into_result()?;
    let too_much = x(&w, &w.alice, "withdraw", json!({"token": w.wnear.id(), "amount": (61 * ONE).to_string()})).await?;
    assert!(fails_with(&too_much, "not enough free balance"), "900 of 960 wNEAR is in the flow");
    let before = balance(&w.wnear, w.alice.id()).await?;
    x(&w, &w.alice, "withdraw", json!({"token": w.wnear.id(), "amount": (60 * ONE).to_string()})).await?.into_result()?;
    assert_eq!(balance(&w.wnear, w.alice.id()).await? - before, 60 * ONE);
    Ok(())
}

#[tokio::test]
async fn only_the_owner_and_keeper_can_run_and_only_the_owner_can_take() -> anyhow::Result<()> {
    let w = setup().await?;
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}});
    let n1 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "manual"}});
    let f = save(&w, json!([n0, n1])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    fund(&w, f, 1, 10 * ONE).await?.into_result()?;
    // Strangers can't run steps, save flows, fund, withdraw, or swap the keeper.
    assert!(fails_with(&step(&w, &w.mallory, f, 0).await?, "owner or keeper only"));
    let bad = json!([{"input": w.wnear.id(), "action": {"label": "steal", "calls": [{"call": "fn", "contract": w.wnear.id(), "method": "ft_transfer",
        "args": format!(r#"{{"receiver_id":"{}","amount":"{{amount}}"}}"#, w.mallory.id()), "deposit": {"kind": "one_yocto"}, "tgas": 10}], "out": null, "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    assert!(fails_with(&x(&w, &w.mallory, "save_flow", json!({"name": "x", "nodes": bad})).await?, "owner only"));
    assert!(fails_with(&x(&w, &w.mallory, "fund", json!({"flow_id": f, "node": 0, "amount": "1"})).await?, "owner only"));
    assert!(fails_with(&x(&w, &w.mallory, "withdraw", json!({"token": w.wnear.id(), "amount": "1"})).await?, "owner only"));
    assert!(fails_with(&x(&w, &w.mallory, "set_keeper", json!({"keeper": w.mallory.id()})).await?, "owner only"));
    assert!(fails_with(&x(&w, &w.keeper, "withdraw", json!({"token": w.wnear.id(), "amount": "1"})).await?, "owner only"));
    assert!(fails_with(&x(&w, &w.keeper, "drain", json!({"flow_id": f})).await?, "owner only"));
    // The keeper runs "now" nodes but not "manual" ones; the owner runs both.
    assert!(fails_with(&step(&w, &w.keeper, f, 1).await?, "OwnerOnly"));
    assert!(step(&w, &w.keeper, f, 0).await?.is_success());
    assert!(step(&w, &w.alice, f, 1).await?.is_success());
    assert_eq!(balance(&w.usdc, &w.vault).await?, 40 * ONE);
    // A recipe can't spend the input twice (two ft_transfer_calls of the same input).
    let twice = json!([{"input": w.wnear.id(), "action": {"label": "x", "calls": [
        {"call": "ft", "receiver": w.dex.id(), "msg": "{}", "tgas": 20}, {"call": "ft", "receiver": w.dex.id(), "msg": "{}", "tgas": 20}],
        "out": null, "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    assert!(x(&w, &w.alice, "save_flow", json!({"name": "x", "nodes": twice})).await?.is_failure());
    Ok(())
}

#[tokio::test]
async fn a_failed_step_keeps_the_money_and_three_failures_pause_the_node() -> anyhow::Result<()> {
    let w = setup().await?;
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 50 * ONE).await?.into_result()?;
    w.alice.call(w.dex.id(), "set_broken").args_json(json!({"broken": true})).transact().await?.into_result()?;
    for _ in 0..3 {
        let r = step(&w, &w.keeper, f, 0).await?;
        assert!(logs(&r).contains("step_failed"), "{}", logs(&r));
    }
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "nothing lost");
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][0]["inbox"], (50 * ONE).to_string());
    assert!(fails_with(&step(&w, &w.keeper, f, 0).await?, "Paused"), "paused after three failures");
    w.alice.call(w.dex.id(), "set_broken").args_json(json!({"broken": false})).transact().await?.into_result()?;
    assert!(x(&w, &w.keeper, "resume", json!({"flow_id": f, "node": 0})).await?.is_failure(), "only the owner resumes");
    x(&w, &w.alice, "resume", json!({"flow_id": f, "node": 0})).await?.into_result()?;
    assert!(logs(&step(&w, &w.keeper, f, 0).await?).contains(r#""event":"step""#));
    assert_eq!(balance(&w.usdc, &w.vault).await?, 100 * ONE);
    assert!(busy(&w).await?.is_null(), "no step left in flight");
    Ok(())
}

#[tokio::test]
async fn slippage_protection_and_price_triggers() -> anyhow::Result<()> {
    let w = setup().await?;
    // Sell wNEAR for USDC only when 1 wNEAR fetches at least 3 USDC, and never accept less than 2.9.
    let mut action = swap(&w, &w.usdc, Some(29 * SCALE / 10));
    action["price"] = json!({"contract": w.dex.id(), "method": "get_return",
        "args": json!({"token_in": w.wnear.id(), "amount_in": ONE.to_string(), "token_out": w.usdc.id()}).to_string(), "per": ONE.to_string()});
    let f = save(&w, json!([{"input": w.wnear.id(), "action": action, "when": {"when": "price_above", "price": (3 * SCALE).to_string()}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(r.is_failure() || !logs(&r).contains(r#""event":"step""#), "price is 2: not yet: {}", logs(&r));
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE);
    assert!(busy(&w).await?.is_null(), "a trigger that didn't fire leaves nothing in flight");
    rate(&w, &w.wnear, &w.usdc, 3 * SCALE).await?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(logs(&r).contains(r#""out":"30000000000000000000""#), "{}", logs(&r));
    // Slippage: a flow that demands 2.5 USDC per wNEAR while the pool pays 2 fails, money stays.
    rate(&w, &w.wnear, &w.usdc, 2 * SCALE).await?;
    let g = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(25 * SCALE / 10)), "when": {"when": "now"}}])).await?;
    fund(&w, g, 0, 10 * ONE).await?.into_result()?;
    assert!(logs(&step(&w, &w.keeper, g, 0).await?).contains("step_failed"));
    assert_eq!(balance(&w.wnear, &w.vault).await?, 990 * ONE);
    Ok(())
}

#[tokio::test]
async fn native_near_stakes_into_a_liquid_token() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(20)).await?.into_result()?;
    let stake = json!({"label": "Stake", "calls": [{"call": "fn", "contract": w.dex.id(), "method": "deposit_and_stake", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": 30}], "out": w.lst.id(), "min_out_per_in": null, "price": null});
    let f = save(&w, json!([{"input": "near", "action": stake, "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 5 * NEAR).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.lst, &w.vault).await?, 5 * NEAR, "5 NEAR in, 5 LST out");
    // Over-funding NEAR the vault doesn't have is refused.
    assert!(fails_with(&fund(&w, f, 0, 1_000 * NEAR).await?, "not enough free NEAR"));
    let _ = &w.worker;
    Ok(())
}

#[tokio::test]
async fn recipes_can_only_call_trusted_contracts_and_never_the_vault_itself() -> anyhow::Result<()> {
    let w = setup().await?;
    let call = |contract: &AccountId, method: &str, args: String| json!([{"input": w.wnear.id(), "action": {"label": "x",
        "calls": [{"call": "ft", "receiver": w.dex.id(), "msg": format!(r#"{{"token_out":"{}","min_out":"0"}}"#, w.usdc.id()), "tgas": 50},
                  {"call": "fn", "contract": contract, "method": method, "args": args, "deposit": {"kind": "none"}, "tgas": 10}],
        "out": w.usdc.id(), "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    // A shared recipe that pokes the vault's own callback (the relay's `cb`) to fake a step's result.
    let r = x(&w, &w.alice, "save_flow", json!({"name": "evil", "nodes": call(&w.vault, "cb", "{}".into())})).await?;
    assert!(fails_with(&r, "untrusted: contract"), "{:?}", r.failures());
    assert!(fails_with(&x(&w, &w.alice, "trust", json!({"add": [{"contract": w.vault, "methods": ["cb"]}], "remove": []})).await?, "a vault never trusts itself"),
        "the vault can't be trusted as a recipe target");
    // Nor the brain (its doors would see the vault as the caller).
    let r = x(&w, &w.alice, "save_flow", json!({"name": "evil", "nodes": call(w.brain.id(), "on_vault", "{}".into())})).await?;
    assert!(fails_with(&r, "untrusted: contract"), "{:?}", r.failures());
    assert!(fails_with(&x(&w, &w.alice, "trust", json!({"add": [{"contract": w.brain.id(), "methods": ["on_vault"]}], "remove": []})).await?, "a vault never trusts the brain"));
    // A recipe that sends tokens to a contract the owner never approved.
    let r = x(&w, &w.alice, "save_flow", json!({"name": "evil", "nodes": call(w.mallory.id(), "anything", "{}".into())})).await?;
    assert!(fails_with(&r, "untrusted: contract"));
    // Moving tokens directly (around the ledger and the spending limit) is never allowed,
    // even on a trusted token, and can't be trusted either.
    let r = x(&w, &w.alice, "save_flow", json!({"name": "evil", "nodes": call(w.wnear.id(), "ft_transfer_call",
        format!(r#"{{"receiver_id":"{}","amount":"1000","msg":""}}"#, w.dex.id()))})).await?;
    assert!(fails_with(&r, "moves funds"), "{:?}", r.failures());
    assert!(fails_with(&x(&w, &w.alice, "trust", json!({"add": [{"contract": w.wnear.id(), "methods": ["ft_transfer"]}], "remove": []})).await?, "moves funds"));
    // A trusted contract, but a method nobody approved.
    let r = x(&w, &w.alice, "save_flow", json!({"name": "evil", "nodes": call(w.dex.id(), "set_broken", "{}".into())})).await?;
    assert!(fails_with(&r, "untrusted: method"), "{:?}", r.failures());
    // A recipe that promises USDC but whose proceeds go elsewhere (here: a swap into a
    // token other than the one it declared) pauses at once instead of repeating.
    let mut liar = swap(&w, &w.lst, None);
    liar["out"] = json!(w.usdc.id());
    rate(&w, &w.wnear, &w.lst, SCALE).await?;
    let l = save(&w, json!([{"input": w.wnear.id(), "action": liar, "when": {"when": "now"}}])).await?;
    fund(&w, l, 0, 20 * ONE).await?.into_result()?;
    set_limit(&w, Some(10 * ONE), 3_600_000_000_000).await?.into_result()?;
    let r = step(&w, &w.keeper, l, 0).await?;
    assert!(fails_with(&r, "spending limit"), "20 wNEAR at once is over the 10/hour limit");
    // Shrink the node's inbox to 5: drain the flow (frees all 20), then fund 5.
    x(&w, &w.alice, "drain", json!({"flow_id": l})).await?.into_result()?;
    fund(&w, l, 0, 5 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, l, 0).await?;
    assert!(r.logs().iter().any(|x| x.contains(r#""paused":true"#)), "{:?}", r.logs());
    fund(&w, l, 0, 5 * ONE).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, l, 0).await?, "Paused"), "it won't run again by itself");
    // Tightening is instant; to lift the limit here, a removal would wait 24h, so leave it (10/h, 5 used).
    // Trust withdrawn after saving: the step refuses to run, the money stays put.
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 5 * ONE).await?.into_result()?;
    x(&w, &w.alice, "trust", json!({"add": [], "remove": [w.dex.id()]})).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(fails_with(&r, "untrusted"), "{:?}", r.failures());
    // 995: the lying recipe above cost one 5 wNEAR step (under the limit) before it paused.
    assert_eq!(balance(&w.wnear, &w.vault).await?, 995 * ONE);
    Ok(())
}

#[tokio::test]
async fn spending_limits_cap_what_flows_can_spend_and_loosening_waits_a_day() -> anyhow::Result<()> {
    let w = setup().await?;
    let minute = 60_000_000_000u64;
    set_limit(&w, Some(15 * ONE), minute).await?.into_result()?;
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    assert!(step(&w, &w.keeper, f, 0).await?.is_success());
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(fails_with(&r, "spending limit: 5000000000000000000"), "{:?}", r.failures());
    assert!(fails_with(&step(&w, &w.alice, f, 0).await?, "spending limit"), "the owner's steps count too");
    // Loosening (or removing) waits 24h; trying to apply it early fails, the old cap holds.
    set_limit(&w, Some(1_000 * ONE), minute).await?.into_result()?;
    set_limit(&w, None, minute).await?.into_result()?;
    let early = x(&w, &w.mallory, "apply_limit", json!({"token": w.wnear.id()})).await?;
    assert!(fails_with(&early, "waits 24h"), "{:?}", early.failures());
    assert!(step(&w, &w.keeper, f, 0).await?.is_failure(), "still capped");
    // Tightening is immediate (and clears the pending loosening).
    set_limit(&w, Some(12 * ONE), 2 * minute).await?.into_result()?;
    let (limits, pending): (Value, Value) = serde_json::from_value(view(&w.brain, &w.vault, "get_limits", Value::Null).await?)?;
    assert_eq!(limits[0]["max"], (12 * ONE).to_string());
    assert_eq!(pending, json!([]));
    // A new period starts once the window has passed.
    for _ in 0..30 {
        w.worker.fast_forward(100).await?;
        if step(&w, &w.keeper, f, 0).await?.is_success() {
            assert_eq!(balance(&w.usdc, &w.vault).await?, 40 * ONE);
            return Ok(());
        }
    }
    anyhow::bail!("the window never reset")
}

#[tokio::test]
async fn a_step_with_too_little_gas_is_refused_up_front_and_never_leaves_the_vault_stuck() -> anyhow::Result<()> {
    // Found on testnet: a step that ran out of gas halfway left the vault "busy".
    // WHY the threshold differs from the full vault's "125 Tgas": on relay + brain
    // the brain sizes the step's rounds itself and names its own figure ("attach at
    // least N Tgas for this step"); we read N off the refusal and try exactly N.
    let w = setup().await?;
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    let r = w.keeper.call(&w.vault, "x").args(json!({"step": {"flow_id": f, "node": 0}}).to_string().into_bytes())
        .gas(Gas::from_tgas(60)).transact().await?;
    let why = format!("{:?}", r.failures());
    assert!(why.contains("attach at least"), "{why}");
    assert!(busy(&w).await?.is_null(), "not stuck");
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "nothing moved");
    let need: u64 = why.split("attach at least ").nth(1).unwrap().split(' ').next().unwrap().parse()?;
    println!("the brain asks for at least {need} Tgas");
    assert!(need <= 300, "a plain swap step fits a mainnet transaction");
    // Just enough gas: it runs and finishes.
    let r = w.keeper.call(&w.vault, "x").args(json!({"step": {"flow_id": f, "node": 0}}).to_string().into_bytes())
        .gas(Gas::from_tgas(need)).transact().await?;
    println!("{:?} {:?}", r.logs(), r.failures());
    assert!(busy(&w).await?.is_null(), "the step finished (or closed itself) either way");
    assert!(logs(&r).contains(r#""event":"step""#), "and with the gas it asked for, it ran: {:?}", r.failures());
    Ok(())
}

const SALT0: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SALT1: &str = "2222222222222222222222222222222222222222222222222222222222222222";

async fn print(w: &World, node: &Value, salt: &str) -> anyhow::Result<String> {
    Ok(w.brain.view("fingerprint_of").args_json(json!({"node": node, "salt": salt})).await?.json()?)
}

async fn step_hidden(w: &World, who: &Account, flow: u32, node: u16, reveal: &Value, salt: &str) -> anyhow::Result<ExecutionFinalResult> {
    x(w, who, "step_hidden", json!({"flow_id": flow, "node": node, "reveal": reveal, "salt": salt})).await
}

#[tokio::test]
async fn a_hidden_flow_shows_nothing_until_a_node_runs_and_can_switch_both_ways() -> anyhow::Result<()> {
    let w = setup().await?;
    // The plan: swap 100 wNEAR to USDC (at least 1.9 each), send it all on, swap back.
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(19 * SCALE / 10)), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]});
    let n1 = json!({"input": w.usdc.id(), "action": swap(&w, &w.wnear, None), "when": {"when": "now"}, "next": []});
    let (p0, p1) = (print(&w, &n0, SALT0).await?, print(&w, &n1, SALT1).await?);
    let f: u32 = x(&w, &w.alice, "save_hidden_flow", json!({"name": "", "inputs": [w.wnear.id(), w.usdc.id()], "fingerprints": [p0, p1]}))
        .await?.into_result()?.json()?;
    // A snooper reading the brain sees the tokens and fingerprints, not where they go or when.
    let seen = flow_state(&w, f).await?.to_string();
    assert!(!seen.contains(w.dex.id().as_str()) && !seen.contains("min_out") && !seen.contains("now"), "{seen}");
    fund(&w, f, 0, 100 * ONE).await?.into_result()?;
    // Wrong ways to run it: as an open flow, with the wrong salt, with a changed node, as a stranger.
    assert!(fails_with(&step(&w, &w.keeper, f, 0).await?, "step_hidden"));
    assert!(step_hidden(&w, &w.keeper, f, 0, &n0, SALT1).await?.is_failure());
    let mut greedy = n0.clone();
    greedy["action"]["min_out_per_in"] = json!("1");
    assert!(step_hidden(&w, &w.keeper, f, 0, &greedy, SALT0).await?.is_failure(), "a changed node doesn't match");
    assert!(step_hidden(&w, &w.mallory, f, 0, &n0, SALT0).await?.is_failure());
    // The keeper reveals node 0: it runs and routes its 200 USDC to node 1, still hidden.
    let r = step_hidden(&w, &w.keeper, f, 0, &n0, SALT0).await?;
    assert!(logs(&r).contains(r#""out":"200000000000000000000""#), "{}", logs(&r));
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (200 * ONE).to_string());
    assert_eq!(s["nodes"], json!([]));
    let alloc = view(&w.brain, &w.vault, "get_allocated", json!({"token": w.usdc.id()})).await?;
    assert_eq!(alloc, json!((200 * ONE).to_string()), "a hidden flow's money is held, not free");
    // Showing it: wrong salts refused; right ones show the real flow, which then runs openly.
    let bad = x(&w, &w.alice, "reveal_flow", json!({"flow_id": f, "nodes": [n0, n1], "salts": [SALT1, SALT0]})).await?;
    assert!(bad.is_failure());
    x(&w, &w.alice, "reveal_flow", json!({"flow_id": f, "nodes": [n0, n1], "salts": [SALT0, SALT1]})).await?.into_result()?;
    assert_eq!(flow_state(&w, f).await?["nodes"][1]["input"], json!(w.usdc.id()));
    // Hiding it again (only the keeper's step_hidden runs it from now on).
    x(&w, &w.alice, "hide_flow", json!({"flow_id": f, "fingerprints": [p0, p1]})).await?.into_result()?;
    assert_eq!(flow_state(&w, f).await?["nodes"], json!([]));
    assert!(step(&w, &w.keeper, f, 1).await?.is_failure());
    assert!(logs(&step_hidden(&w, &w.keeper, f, 1, &n1, SALT1).await?).contains(r#""event":"step""#));
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "100 wNEAR -> 200 USDC -> 100 wNEAR");
    Ok(())
}

#[tokio::test]
async fn a_hidden_node_is_checked_like_a_saved_one_when_it_runs() -> anyhow::Result<()> {
    let w = setup().await?;
    // The owner committed to nodes that would never pass save_flow: they're refused when revealed.
    let wrong_route = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]});
    let steal = json!({"input": w.wnear.id(), "action": {"label": "steal", "calls": [{"call": "ft", "receiver": w.mallory.id(), "msg": "", "tgas": 20}],
        "out": null, "min_out_per_in": null, "price": null}, "when": {"when": "now"}, "next": []});
    let filler = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}, "next": []});
    let prints = vec![print(&w, &wrong_route, SALT0).await?, print(&w, &steal, SALT1).await?];
    let f: u32 = x(&w, &w.alice, "save_hidden_flow", json!({"name": "", "inputs": [w.wnear.id(), w.wnear.id()], "fingerprints": prints}))
        .await?.into_result()?.json()?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    fund(&w, f, 1, 10 * ONE).await?.into_result()?;
    let r = step_hidden(&w, &w.keeper, f, 0, &wrong_route, SALT0).await?;
    assert!(fails_with(&r, "RouteTokenMismatch"), "USDC can't flow into a wNEAR node: {:?}", r.failures());
    let r = step_hidden(&w, &w.alice, f, 1, &steal, SALT1).await?;
    assert!(fails_with(&r, "untrusted"), "tokens only go to trusted contracts: {:?}", r.failures());
    // Untrusted input tokens are refused at save time, like any flow.
    let r = x(&w, &w.alice, "save_hidden_flow", json!({"name": "", "inputs": [w.mallory.id()], "fingerprints": [print(&w, &filler, SALT0).await?]})).await?;
    assert!(r.is_failure());
    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "nothing moved");
    assert!(busy(&w).await?.is_null());
    Ok(())
}
