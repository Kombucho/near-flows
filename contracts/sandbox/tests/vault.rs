//! Vault integration tests: a user's own vault running flows in a local NEAR
//! node, against a test exchange and staking pool (mock_dex) with fixed rates.
//! Build the wasm first: make build

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals)
const SCALE: u128 = 1_000_000_000_000_000_000_000_000; // rates are scaled 1e24
const NEAR: u128 = SCALE; // 1 NEAR in yocto

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

struct World {
    worker: Worker<Sandbox>,
    alice: Account,
    keeper: Account,
    mallory: Account,
    wnear: Contract,
    usdc: Contract,
    lst: Contract,
    dex: Contract,
    vault: Contract,
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

/// Alice owns a vault holding 1,000 wNEAR; the dex holds reserves of everything.
async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.dev_create_account().await?;
    let (alice, keeper, mallory) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let (wnear, usdc, lst) = (token(&worker, &root, "wNEAR").await?, token(&worker, &root, "USDC").await?, token(&worker, &root, "LST").await?);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc, &lst] {
        for who in [alice.id(), dex.id(), vault.id(), mallory.id()] {
            register(&root, t, who).await?;
        }
        give(&root, t, dex.id(), 100_000_000 * ONE).await?;
    }
    give(&root, &wnear, alice.id(), 10_000 * ONE).await?;
    give(&alice, &wnear, vault.id(), 1_000 * ONE).await?;
    alice.call(vault.id(), "trust").args_json(json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": lst.id()},
        {"contract": dex.id(), "methods": ["deposit_and_stake", "get_return"]}], "remove": []}))
        .transact().await?.into_result()?;
    let w = World { worker, alice, keeper, mallory, wnear, usdc, lst, dex, vault };
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
    Ok(w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "test", "nodes": nodes})).max_gas().transact().await?.into_result()?.json()?)
}

async fn fund(w: &World, flow: u32, node: u16, amount: u128) -> anyhow::Result<ExecutionFinalResult> {
    Ok(w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": flow, "node": node, "amount": amount.to_string()})).max_gas().transact().await?)
}

async fn step(w: &World, who: &Account, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    let r = who.call(w.vault.id(), "step").args_json(json!({"flow_id": flow, "node": node})).max_gas().transact().await?;
    println!("GAS step: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    for o in r.receipt_outcomes() {
        if o.is_failure() || !o.logs.is_empty() {
            println!("  {} {:?} {:?}", o.executor_id, o.logs, o.clone().into_result().err());
        }
    }
    Ok(r)
}

async fn flow_state(w: &World, flow: u32) -> anyhow::Result<Value> {
    Ok(w.vault.view("get_flow").args_json(json!({"flow_id": flow})).await?.json()?)
}

fn logs(r: &ExecutionFinalResult) -> String {
    r.logs().join("\n")
}

#[tokio::test]
async fn a_swap_splits_its_output_down_the_flow() -> anyhow::Result<()> {
    let w = setup().await?;
    // 100 wNEAR -> USDC; 60% of the USDC goes to a second swap back to wNEAR, 40% stays free.
    let mut n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(19 * SCALE / 10)), "when": {"when": "now"}, "next": [{"to": 1, "bps": 6000}]});
    n0["next"] = json!([{"to": 1, "bps": 6000}]);
    let n1 = json!({"input": w.usdc.id(), "action": swap(&w, &w.wnear, None), "when": {"when": "now"}});
    let f = save(&w, json!([n0, n1])).await?;
    fund(&w, f, 0, 100 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(logs(&r).contains(r#""out":"200000000000000000000""#), "{}", logs(&r));
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (120 * ONE).to_string(), "60% of 200 USDC routed on");
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, 200 * ONE);
    let alloc: String = w.vault.view("get_allocated").args_json(json!({"token": w.usdc.id()})).await?.json()?;
    assert_eq!(alloc, (120 * ONE).to_string(), "the other 80 USDC is free");
    // Second hop: 120 USDC -> 60 wNEAR, nothing routed, all free.
    assert!(step(&w, &w.keeper, f, 1).await?.is_success());
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 960 * ONE);
    // Alice withdraws free money; she can't withdraw what a flow holds.
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": (900 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
    let too_much = w.alice.call(w.vault.id(), "withdraw").args_json(json!({"token": w.wnear.id(), "amount": (61 * ONE).to_string()})).max_gas().transact().await?;
    assert!(too_much.is_failure(), "900 of 960 wNEAR is in the flow");
    let before = balance(&w.wnear, w.alice.id()).await?;
    w.alice.call(w.vault.id(), "withdraw").args_json(json!({"token": w.wnear.id(), "amount": (60 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
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
    assert!(step(&w, &w.mallory, f, 0).await?.is_failure());
    let bad = json!([{"input": w.wnear.id(), "action": {"label": "steal", "calls": [{"call": "fn", "contract": w.wnear.id(), "method": "ft_transfer",
        "args": format!(r#"{{"receiver_id":"{}","amount":"{{amount}}"}}"#, w.mallory.id()), "deposit": {"kind": "one_yocto"}, "tgas": 10}], "out": null, "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    assert!(w.mallory.call(w.vault.id(), "save_flow").args_json(json!({"name": "x", "nodes": bad})).max_gas().transact().await?.is_failure());
    assert!(w.mallory.call(w.vault.id(), "withdraw").args_json(json!({"token": w.wnear.id(), "amount": "1"})).max_gas().transact().await?.is_failure());
    assert!(w.mallory.call(w.vault.id(), "set_keeper").args_json(json!({"keeper": w.mallory.id()})).transact().await?.is_failure());
    assert!(w.keeper.call(w.vault.id(), "withdraw").args_json(json!({"token": w.wnear.id(), "amount": "1"})).max_gas().transact().await?.is_failure());
    assert!(w.keeper.call(w.vault.id(), "drain").args_json(json!({"flow_id": f})).transact().await?.is_failure());
    // The keeper runs "now" nodes but not "manual" ones; the owner runs both.
    assert!(step(&w, &w.keeper, f, 1).await?.is_failure());
    assert!(step(&w, &w.keeper, f, 0).await?.is_success());
    assert!(step(&w, &w.alice, f, 1).await?.is_success());
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, 40 * ONE);
    // A recipe can't spend the input twice (ft_transfer_call AND a NEAR deposit), and NEAR can't be "ft".
    let twice = json!([{"input": w.wnear.id(), "action": {"label": "x", "calls": [
        {"call": "ft", "receiver": w.dex.id(), "msg": "{}", "tgas": 20}, {"call": "ft", "receiver": w.dex.id(), "msg": "{}", "tgas": 20}],
        "out": null, "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    assert!(w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "x", "nodes": twice})).max_gas().transact().await?.is_failure());
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
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 1_000 * ONE, "nothing lost");
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][0]["inbox"], (50 * ONE).to_string());
    assert!(step(&w, &w.keeper, f, 0).await?.is_failure(), "paused after three failures");
    w.alice.call(w.dex.id(), "set_broken").args_json(json!({"broken": false})).transact().await?.into_result()?;
    w.alice.call(w.vault.id(), "resume").args_json(json!({"flow_id": f, "node": 0})).transact().await?.into_result()?;
    assert!(logs(&step(&w, &w.keeper, f, 0).await?).contains(r#""event":"step""#));
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, 100 * ONE);
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
    assert!(step(&w, &w.keeper, f, 0).await?.is_failure(), "price is 2: not yet");
    rate(&w, &w.wnear, &w.usdc, 3 * SCALE).await?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(logs(&r).contains(r#""out":"30000000000000000000""#), "{}", logs(&r));
    // Slippage: a flow that demands 2.5 USDC per wNEAR while the pool pays 2 fails, money stays.
    rate(&w, &w.wnear, &w.usdc, 2 * SCALE).await?;
    let g = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(25 * SCALE / 10)), "when": {"when": "now"}}])).await?;
    fund(&w, g, 0, 10 * ONE).await?.into_result()?;
    assert!(logs(&step(&w, &w.keeper, g, 0).await?).contains("step_failed"));
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 990 * ONE);
    Ok(())
}

#[tokio::test]
async fn native_near_stakes_into_a_liquid_token() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(20)).await?.into_result()?;
    let stake = json!({"label": "Stake", "calls": [{"call": "fn", "contract": w.dex.id(), "method": "deposit_and_stake", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": 30}], "out": w.lst.id(), "min_out_per_in": null, "price": null});
    let f = save(&w, json!([{"input": "near", "action": stake, "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 5 * NEAR).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.lst, w.vault.id()).await?, 5 * NEAR, "5 NEAR in, 5 LST out");
    // Over-funding NEAR the vault doesn't have is refused.
    assert!(fund(&w, f, 0, 1_000 * NEAR).await?.is_failure());
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
    // A shared recipe that pokes the vault's own callback to fake a step's result.
    let r = w.alice.call(w.vault.id(), "save_flow")
        .args_json(json!({"name": "evil", "nodes": call(w.vault.id(), "on_measured", r#"{"before":false}"#.into())})).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("untrusted: contract"), "{:?}", r.failures());
    let r = w.alice.call(w.vault.id(), "trust").args_json(json!({"add": [{"contract": w.vault.id(), "methods": ["on_measured"]}], "remove": []})).transact().await?;
    assert!(r.is_failure(), "the vault can't be trusted as a recipe target");
    // A recipe that sends tokens to a contract the owner never approved.
    let r = w.alice.call(w.vault.id(), "save_flow")
        .args_json(json!({"name": "evil", "nodes": call(w.mallory.id(), "anything", "{}".into())})).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("untrusted: contract"));
    // Moving tokens directly (around the ledger and the spending limit) is never allowed,
    // even on a trusted token, and can't be trusted either.
    let r = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "evil", "nodes": call(w.wnear.id(), "ft_transfer_call",
        format!(r#"{{"receiver_id":"{}","amount":"1000","msg":""}}"#, w.dex.id()))})).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("moves funds"), "{:?}", r.failures());
    let r = w.alice.call(w.vault.id(), "trust").args_json(json!({"add": [{"contract": w.wnear.id(), "methods": ["ft_transfer"]}], "remove": []})).transact().await?;
    assert!(format!("{:?}", r.failures()).contains("moves funds"));
    // A trusted contract, but a method nobody approved.
    let r = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "evil", "nodes": call(w.dex.id(), "set_broken", "{}".into())})).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("untrusted: method"), "{:?}", r.failures());
    // A recipe that promises USDC but whose proceeds go elsewhere (here: a swap into a
    // token other than the one it declared) pauses at once instead of repeating.
    let mut liar = swap(&w, &w.lst, None);
    liar["out"] = json!(w.usdc.id());
    w.alice.call(w.dex.id(), "set_rate").args_json(json!({"token_in": w.wnear.id(), "token_out": w.lst.id(), "rate": SCALE.to_string()})).transact().await?.into_result()?;
    let l = save(&w, json!([{"input": w.wnear.id(), "action": liar, "when": {"when": "now"}}])).await?;
    fund(&w, l, 0, 20 * ONE).await?.into_result()?;
    w.alice.call(w.vault.id(), "set_limit").args_json(json!({"token": w.wnear.id(), "max": (10 * ONE).to_string(), "window_ns": 3_600_000_000_000u64})).transact().await?.into_result()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": l, "node": 0, "amount": "1"})).max_gas().transact().await?;
    let r = step(&w, &w.keeper, l, 0).await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit"), "20 wNEAR at once is over the 10/hour limit");
    w.alice.call(w.vault.id(), "set_limit").args_json(json!({"token": w.wnear.id(), "max": (25 * ONE).to_string(), "window_ns": 3_600_000_000_000u64})).transact().await?;
    let _ = w.alice.call(w.vault.id(), "drain").args_json(json!({"flow_id": l})).transact().await?;
    w.alice.call(w.vault.id(), "set_limit").args_json(json!({"token": w.wnear.id(), "max": (10 * ONE).to_string(), "window_ns": 3_600_000_000_000u64})).transact().await?.into_result()?;
    fund(&w, l, 0, 5 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, l, 0).await?;
    assert!(r.logs().iter().any(|x| x.contains(r#""paused":true"#)), "{:?}", r.logs());
    fund(&w, l, 0, 5 * ONE).await?.into_result()?;
    assert!(format!("{:?}", step(&w, &w.keeper, l, 0).await?.failures()).contains("Paused"), "it won't run again by itself");
    w.alice.call(w.vault.id(), "set_limit").args_json(json!({"token": w.wnear.id(), "max": null, "window_ns": 0})).transact().await?.into_result()?;
    // Trust withdrawn after saving: the step refuses to run, the money stays put.
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    w.alice.call(w.vault.id(), "trust").args_json(json!({"add": [], "remove": [w.dex.id()]})).transact().await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(format!("{:?}", r.failures()).contains("untrusted"));
    // 995: the lying recipe above cost one 5 wNEAR step (under the limit) before it paused.
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 995 * ONE);
    Ok(())
}

#[tokio::test]
async fn spending_limits_cap_what_flows_can_spend_and_loosening_waits_a_day() -> anyhow::Result<()> {
    let w = setup().await?;
    let minute = 60_000_000_000u64;
    let set = |max: Option<u128>, window: u64| w.alice.call(w.vault.id(), "set_limit")
        .args_json(json!({"token": w.wnear.id(), "max": max.map(|m| m.to_string()), "window_ns": window})).transact();
    set(Some(15 * ONE), minute).await?.into_result()?;
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    assert!(step(&w, &w.keeper, f, 0).await?.is_success());
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0).await?;
    assert!(format!("{:?}", r.failures()).contains("spending limit: 5000000000000000000"), "{:?}", r.failures());
    assert!(step(&w, &w.alice, f, 0).await?.is_failure(), "the owner's steps count too");
    // Loosening (or removing) waits 24h; trying to apply it early fails, the old cap holds.
    set(Some(1_000 * ONE), minute).await?.into_result()?;
    set(None, minute).await?.into_result()?;
    let early = w.mallory.call(w.vault.id(), "apply_limit").args_json(json!({"token": w.wnear.id()})).transact().await?;
    assert!(format!("{:?}", early.failures()).contains("waits 24h"));
    assert!(step(&w, &w.keeper, f, 0).await?.is_failure(), "still capped");
    // Tightening is immediate (and clears the pending loosening).
    set(Some(12 * ONE), 2 * minute).await?.into_result()?;
    let (limits, pending): (Value, Value) = w.vault.view("get_limits").await?.json()?;
    assert_eq!(limits[0]["max"], (12 * ONE).to_string());
    assert_eq!(pending, json!([]));
    // A new period starts once the window has passed.
    for _ in 0..30 {
        w.worker.fast_forward(100).await?;
        if step(&w, &w.keeper, f, 0).await?.is_success() {
            assert_eq!(balance(&w.usdc, w.vault.id()).await?, 40 * ONE);
            return Ok(());
        }
    }
    anyhow::bail!("the window never reset")
}

#[tokio::test]
async fn a_step_with_too_little_gas_is_refused_up_front_and_never_leaves_the_vault_stuck() -> anyhow::Result<()> {
    // Found on testnet: a step that ran out of gas halfway left the vault "busy".
    let w = setup().await?;
    let f = save(&w, json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    let r = w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": f, "node": 0}))
        .gas(near_workspaces::types::Gas::from_tgas(100)).transact().await?;
    assert!(format!("{:?}", r.failures()).contains("attach at least 125 Tgas"), "{:?}", r.failures());
    let busy: Option<Value> = w.vault.view("get_busy").await?.json()?;
    assert!(busy.is_none(), "not stuck");
    // Just enough gas: it runs and finishes.
    let r = w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": f, "node": 0}))
        .gas(near_workspaces::types::Gas::from_tgas(130)).transact().await?;
    println!("{:?}", r.logs());
    let busy: Option<Value> = w.vault.view("get_busy").await?.json()?;
    assert!(busy.is_none(), "the step finished (or closed itself) either way");
    Ok(())
}

const SALT0: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const SALT1: &str = "2222222222222222222222222222222222222222222222222222222222222222";

async fn print(w: &World, node: &Value, salt: &str) -> anyhow::Result<String> {
    Ok(w.vault.view("fingerprint_of").args_json(json!({"node": node, "salt": salt})).await?.json()?)
}

async fn step_hidden(w: &World, who: &Account, flow: u32, node: u16, reveal: &Value, salt: &str) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(w.vault.id(), "step_hidden").args_json(json!({"flow_id": flow, "node": node, "reveal": reveal, "salt": salt})).max_gas().transact().await?)
}

#[tokio::test]
async fn a_hidden_flow_shows_nothing_until_a_node_runs_and_can_switch_both_ways() -> anyhow::Result<()> {
    let w = setup().await?;
    // The plan: swap 100 wNEAR to USDC (at least 1.9 each), send it all on, swap back.
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(19 * SCALE / 10)), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]});
    let n1 = json!({"input": w.usdc.id(), "action": swap(&w, &w.wnear, None), "when": {"when": "now"}, "next": []});
    let (p0, p1) = (print(&w, &n0, SALT0).await?, print(&w, &n1, SALT1).await?);
    let f: u32 = w.alice.call(w.vault.id(), "save_hidden_flow")
        .args_json(json!({"name": "", "inputs": [w.wnear.id(), w.usdc.id()], "fingerprints": [p0, p1]}))
        .max_gas().transact().await?.into_result()?.json()?;
    // A snooper reading the vault sees the tokens and fingerprints, not where they go or when.
    let seen = flow_state(&w, f).await?.to_string();
    assert!(!seen.contains(w.dex.id().as_str()) && !seen.contains("min_out") && !seen.contains("now"), "{seen}");
    fund(&w, f, 0, 100 * ONE).await?.into_result()?;
    // Wrong ways to run it: as an open flow, with the wrong salt, with a changed node, as a stranger.
    assert!(step(&w, &w.keeper, f, 0).await?.is_failure());
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
    let alloc: String = w.vault.view("get_allocated").args_json(json!({"token": w.usdc.id()})).await?.json()?;
    assert_eq!(alloc, (200 * ONE).to_string(), "a hidden flow's money is held, not free");
    // Showing it: wrong salts refused; right ones show the real flow, which then runs openly.
    let bad = w.alice.call(w.vault.id(), "reveal_flow").args_json(json!({"flow_id": f, "nodes": [n0, n1], "salts": [SALT1, SALT0]})).max_gas().transact().await?;
    assert!(bad.is_failure());
    w.alice.call(w.vault.id(), "reveal_flow").args_json(json!({"flow_id": f, "nodes": [n0, n1], "salts": [SALT0, SALT1]})).max_gas().transact().await?.into_result()?;
    assert_eq!(flow_state(&w, f).await?["nodes"][1]["input"], json!(w.usdc.id()));
    // Hiding it again (only the keeper's step_hidden runs it from now on).
    w.alice.call(w.vault.id(), "hide_flow").args_json(json!({"flow_id": f, "fingerprints": [p0, p1]})).max_gas().transact().await?.into_result()?;
    assert_eq!(flow_state(&w, f).await?["nodes"], json!([]));
    assert!(step(&w, &w.keeper, f, 1).await?.is_failure());
    assert!(logs(&step_hidden(&w, &w.keeper, f, 1, &n1, SALT1).await?).contains(r#""event":"step""#));
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 1_000 * ONE, "100 wNEAR -> 200 USDC -> 100 wNEAR");
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
    let f: u32 = w.alice.call(w.vault.id(), "save_hidden_flow")
        .args_json(json!({"name": "", "inputs": [w.wnear.id(), w.wnear.id()], "fingerprints": prints}))
        .max_gas().transact().await?.into_result()?.json()?;
    fund(&w, f, 0, 10 * ONE).await?.into_result()?;
    fund(&w, f, 1, 10 * ONE).await?.into_result()?;
    let r = step_hidden(&w, &w.keeper, f, 0, &wrong_route, SALT0).await?;
    assert!(r.is_failure() && format!("{:?}", r.failures()).contains("RouteTokenMismatch"), "USDC can't flow into a wNEAR node");
    let r = step_hidden(&w, &w.alice, f, 1, &steal, SALT1).await?;
    assert!(r.is_failure() && format!("{:?}", r.failures()).contains("untrusted"), "tokens only go to trusted contracts");
    // Untrusted input tokens are refused at save time, like any flow.
    let r = w.alice.call(w.vault.id(), "save_hidden_flow")
        .args_json(json!({"name": "", "inputs": [w.mallory.id()], "fingerprints": [print(&w, &filler, SALT0).await?]})).max_gas().transact().await?;
    assert!(r.is_failure());
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 1_000 * ONE, "nothing moved");
    Ok(())
}
