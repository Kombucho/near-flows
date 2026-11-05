//! The vault against REAL protocol code: Rhea DEX, LiNEAR and wNEAR, as
//! deployed on testnet (fetched by scripts/fetch-protocols.sh), running in a
//! local NEAR node. Build our wasm first: make build

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 USDC in the mock (18 decimals)
const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const SCALE: u128 = NEAR; // prices are scaled 1e24

fn ours(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

fn theirs(id: &str) -> Vec<u8> {
    let path = format!("{}/protocols/{id}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run scripts/fetch-protocols.sh first"))
}

struct World {
    worker: Worker<Sandbox>,
    lp: Account,
    alice: Account,
    keeper: Account,
    wrap: Contract,
    usdc: Contract,
    rhea: Contract,
    linear: Contract,
    vault: Contract,
    pool: u64,
}

async fn register(payer: &Account, token: &AccountId, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token, "storage_deposit").args_json(json!({"account_id": who, "registration_only": true}))
        .deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

/// Real wNEAR, Rhea and LiNEAR; a pool of 1,000 wNEAR : 2,000 USDC (1 NEAR = 2 USDC);
/// Alice's vault holds 100 NEAR and trusts all of them.
async fn setup() -> anyhow::Result<World> {
    let worker: Worker<Sandbox> = near_workspaces::sandbox().await?;
    let lp = worker.root_account()?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);

    let wrap = worker.dev_deploy(&theirs("wrap.testnet")).await?;
    wrap.call("new").transact().await?.into_result()?;
    let usdc = worker.dev_deploy(&ours("mock_ft")).await?;
    usdc.call("new").args_json(json!({"owner_id": lp.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
        "name": "Mock USDC", "symbol": "USDC", "decimals": 18, "call_fee_bps": 0})).transact().await?.into_result()?;
    let rhea = worker.dev_deploy(&theirs("ref-finance-101.testnet")).await?;
    rhea.call("new").args_json(json!({"owner_id": rhea.id(), "exchange_fee": 4, "referral_fee": 1, "boost_farm_id": "boostfarm.test.near", "burrowland_id": "burrow.test.near", "wnear_id": wrap.id()})).transact().await?.into_result()?;
    let linear = worker.dev_deploy(&theirs("linear-protocol.testnet")).await?;
    linear.call("new").args_json(json!({"owner_id": linear.id()})).max_gas().transact().await?.into_result()?;

    // The liquidity provider opens a wNEAR/USDC pool on Rhea.
    lp.call(wrap.id(), "storage_deposit").args_json(json!({})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    lp.call(wrap.id(), "near_deposit").deposit(NearToken::from_near(1_000)).transact().await?.into_result()?;
    for t in [wrap.id(), usdc.id()] {
        register(&lp, t, rhea.id()).await?;
    }
    rhea.call("extend_whitelisted_tokens").args_json(json!({"tokens": [wrap.id(), usdc.id()]})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    let pool: u64 = lp.call(rhea.id(), "add_simple_pool").args_json(json!({"tokens": [wrap.id(), usdc.id()], "fee": 25}))
        .deposit(NearToken::from_millinear(100)).transact().await?.json()?;
    lp.call(rhea.id(), "storage_deposit").args_json(json!({})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
    lp.call(rhea.id(), "register_tokens").args_json(json!({"token_ids": [wrap.id(), usdc.id()]})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    for (t, a) in [(wrap.id(), 1_000 * NEAR), (usdc.id(), 2_000 * ONE)] {
        lp.call(t, "ft_transfer_call").args_json(json!({"receiver_id": rhea.id(), "amount": a.to_string(), "msg": ""}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    }
    lp.call(rhea.id(), "add_liquidity").args_json(json!({"pool_id": pool, "amounts": [(1_000 * NEAR).to_string(), (2_000 * ONE).to_string()]}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;

    // Alice's vault.
    let vault = worker.dev_deploy(&ours("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    for t in [wrap.id(), usdc.id()] {
        register(&lp, t, vault.id()).await?;
    }
    alice.call(vault.id(), "trust").args_json(json!({"add": [
        {"contract": wrap.id(), "methods": ["near_deposit"]}, {"contract": usdc.id()},
        {"contract": rhea.id(), "methods": ["get_return"]}, {"contract": linear.id(), "methods": ["deposit_and_stake"]}], "remove": []})).transact().await?.into_result()?;
    let w = World { worker, lp, alice, keeper, wrap, usdc, rhea, linear, vault, pool };
    Ok(w)
}

fn wrap_near(w: &World) -> Value {
    json!({"label": "Wrap NEAR", "calls": [{"call": "fn", "contract": w.wrap.id(), "method": "near_deposit", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": 10}], "out": w.wrap.id(), "min_out_per_in": null, "price": null})
}

/// A Rhea v1 instant swap, exactly the msg the Rhea SDK builds.
fn rhea_swap(w: &World, from: &Contract, to: &Contract, min_out_per_in: Option<u128>) -> Value {
    let msg = format!(r#"{{"force":0,"actions":[{{"pool_id":{},"token_in":"{}","token_out":"{}","amount_in":"{{amount}}","min_amount_out":"{{min_out}}"}}],"skip_unwrap_near":true}}"#,
        w.pool, from.id(), to.id());
    json!({"label": "Swap on Rhea", "calls": [{"call": "ft", "receiver": w.rhea.id(), "msg": msg, "tgas": 120}],
           "out": to.id(), "min_out_per_in": min_out_per_in.map(|m| m.to_string()),
           "price": {"contract": w.rhea.id(), "method": "get_return",
                     "args": json!({"pool_id": w.pool, "token_in": from.id(), "amount_in": NEAR.to_string(), "token_out": to.id()}).to_string(),
                     "per": NEAR.to_string()}})
}

fn linear_stake(w: &World) -> Value {
    json!({"label": "Stake with LiNEAR", "calls": [{"call": "fn", "contract": w.linear.id(), "method": "deposit_and_stake", "args": "{}",
        "deposit": {"kind": "input"}, "tgas": 50}], "out": w.linear.id(), "min_out_per_in": null, "price": null})
}

async fn save(w: &World, nodes: Value) -> anyhow::Result<u32> {
    let r = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "real", "nodes": nodes})).max_gas().transact().await?;
    Ok(r.into_result()?.json()?)
}

async fn fund(w: &World, flow: u32, node: u16, amount: u128) -> anyhow::Result<()> {
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": flow, "node": node, "amount": amount.to_string()})).max_gas().transact().await?.into_result()?;
    Ok(())
}

async fn step(w: &World, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    let r = w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": flow, "node": node})).max_gas().transact().await?;
    println!("GAS step: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    for o in r.receipt_outcomes() {
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

#[tokio::test]
async fn near_is_wrapped_then_half_is_swapped_on_rhea() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(50)).await?.into_result()?;
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 5000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, Some(19 * SCALE / 10 / 1_000_000)), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    let r = step(&w, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.wrap, w.vault.id()).await?, 10 * NEAR, "10 NEAR wrapped");
    let r = step(&w, f, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?}", r.logs());
    let usdc = balance(&w.usdc, w.vault.id()).await?;
    // 5 wNEAR into a 1,000 : 2,000 pool with a 0.25% fee: a bit under 10 USDC.
    println!("5 wNEAR -> {:.4} USDC on Rhea", usdc as f64 / ONE as f64);
    assert!(usdc > 9_9 * ONE / 10 && usdc < 10 * ONE, "{usdc}");
    assert_eq!(balance(&w.wrap, w.vault.id()).await?, 5 * NEAR, "the unrouted half stays free");
    Ok(())
}

#[tokio::test]
async fn rhea_refuses_a_swap_below_the_minimum_and_the_money_stays_in_the_flow() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(20)).await?.into_result()?;
    // Demand 2.5 USDC per wNEAR from a pool paying ~2.
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, Some(25 * SCALE / 10 / 1_000_000)), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 5 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")), "{:?}", r.logs());
    assert_eq!(balance(&w.wrap, w.vault.id()).await?, 5 * NEAR, "Rhea refunded, nothing lost");
    let s: Value = w.vault.view("get_flow").args_json(json!({"flow_id": f})).await?.json()?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (5 * NEAR).to_string(), "still waiting in the swap node");
    Ok(())
}

#[tokio::test]
async fn a_price_trigger_reads_rhea_and_waits() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(20)).await?.into_result()?;
    // Sell wNEAR only when Rhea pays 3+ USDC per wNEAR (it pays ~2).
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, None), "when": {"when": "price_above", "price": (3 * SCALE / 1_000_000).to_string()}},
    ])).await?;
    fund(&w, f, 0, 5 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(format!("{:?}", r.failures()).contains("NotYet"), "{:?}", r.failures());
    assert_eq!(balance(&w.wrap, w.vault.id()).await?, 5 * NEAR);
    Ok(())
}

#[tokio::test]
async fn native_near_is_staked_with_real_linear() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(20)).await?.into_result()?;
    let f = save(&w, json!([{"input": "near", "action": linear_stake(&w), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    let r = step(&w, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let got = balance(&w.linear, w.vault.id()).await?;
    let price: String = w.linear.view("ft_price").await?.json()?;
    println!("10 NEAR -> {:.4} LiNEAR (price {price})", got as f64 / NEAR as f64);
    assert!(got > 9 * NEAR && got <= 10 * NEAR, "{got}");
    let _ = (&w.usdc, &w.rhea, &w.worker, &w.lp);
    Ok(())
}


/// Real Rhea lending (its testnet build), configured like testnet: fixed default
/// prices (wNEAR $9.60, USDC $1.00), so no oracle is needed in a local node.
/// The liquidity provider supplies 5,000 USDC for others to borrow.
async fn lending(w: &World) -> anyhow::Result<Contract> {
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
    for who in [w.lp.id(), w.vault.id()] {
        w.lp.call(lend.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(100))
            .transact().await?.into_result()?;
    }
    w.lp.call(w.usdc.id(), "ft_transfer_call").args_json(json!({"receiver_id": lend.id(), "amount": (5_000 * ONE).to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    w.alice.call(w.vault.id(), "trust").args_json(json!({"add": [{"contract": lend.id(), "methods": ["execute_with_pyth"]}], "remove": []})).transact().await?.into_result()?;
    Ok(lend)
}

/// "Supply wNEAR as collateral and borrow USDC against it": one node, two calls.
/// `usdc_per_wnear` sets how much to borrow per wNEAR supplied ({scaled}).
fn supply_and_borrow(w: &World, lend: &Contract, usdc_per_wnear: u128) -> Value {
    let supply = format!(r#"{{"Execute":{{"actions":[{{"IncreaseCollateral":{{"token_id":"{}","amount":null,"max_amount":null}}}}]}}}}"#, w.wrap.id());
    let borrow = format!(r#"{{"actions":[{{"Borrow":{{"token_id":"{u}","amount":"{{scaled}}","max_amount":null}}}},{{"Withdraw":{{"token_id":"{u}","amount":null,"max_amount":"{{scaled}}"}}}}]}}"#, u = w.usdc.id());
    json!({"label": "Borrow USDC on Rhea", "calls": [
            {"call": "ft", "receiver": lend.id(), "msg": supply, "tgas": 60},
            {"call": "fn", "contract": lend.id(), "method": "execute_with_pyth", "args": borrow, "deposit": {"kind": "one_yocto"}, "tgas": 120}],
           "out": w.usdc.id(), "min_out_per_in": null, "price": null, "scale_per_in": usdc_per_wnear.to_string() /* per 1 wNEAR = per 1e24 = the scale */})
}

/// "Repay USDC and take wNEAR collateral back": `wnear_per_usdc` (scaled 1e24) sets
/// how much collateral to release per USDC repaid.
fn repay_and_release(w: &World, lend: &Contract, wnear_per_usdc: u128) -> Value {
    // The testnet build predates the "OnlyRepay" shortcut: use Execute { Repay }.
    let repay = format!(r#"{{"Execute":{{"actions":[{{"Repay":{{"token_id":"{}","amount":null,"max_amount":null}}}}]}}}}"#, w.usdc.id());
    let release = format!(r#"{{"actions":[{{"DecreaseCollateral":{{"token_id":"{t}","amount":"{{scaled}}","max_amount":null}}}},{{"Withdraw":{{"token_id":"{t}","amount":null,"max_amount":"{{scaled}}"}}}}]}}"#, t = w.wrap.id());
    json!({"label": "Repay on Rhea", "calls": [
            {"call": "ft", "receiver": lend.id(), "msg": repay, "tgas": 60},
            {"call": "fn", "contract": lend.id(), "method": "execute_with_pyth", "args": release, "deposit": {"kind": "one_yocto"}, "tgas": 120}],
           "out": w.wrap.id(), "min_out_per_in": null, "price": null, "scale_per_in": wnear_per_usdc.to_string()})
}

#[tokio::test]
async fn borrow_usdc_against_wnear_on_real_rhea_lending() -> anyhow::Result<()> {
    let w = setup().await?;
    let lend = lending(&w).await?;
    w.alice.transfer_near(w.vault.id(), NearToken::from_near(30)).await?.into_result()?;
    // 10 NEAR -> wNEAR -> collateral; borrow 3 USDC per wNEAR (worth $9.60, so ~31% LTV).
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": supply_and_borrow(&w, &lend, 3 * ONE), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?}", r.logs());
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, 30 * ONE, "borrowed 30 USDC into the vault");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": w.vault.id()})).await?.json()?;
    println!("vault on Rhea lending: {acct}");
    assert_eq!(acct["collateral"][0]["balance"], (10 * NEAR).to_string());

    // A second borrow after interest has started (shares no longer 1:1).
    let g = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": supply_and_borrow(&w, &lend, 3 * ONE), "when": {"when": "now"}},
    ])).await?;
    fund(&w, g, 0, 5 * NEAR).await?;
    step(&w, g, 0).await?.into_result()?;
    assert!(step(&w, g, 1).await?.logs().iter().any(|l| l.contains(r#""event":"step""#)));
    let usdc = balance(&w.usdc, w.vault.id()).await?;
    assert!(usdc > 45 * ONE - 1_000 && usdc <= 45 * ONE, "{usdc}");

    // Too greedy: 50 USDC per wNEAR against $9.60 of collateral. Rhea refuses the
    // loan; the wNEAR stays supplied as collateral (it left the flow), nothing borrowed.
    let h = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": supply_and_borrow(&w, &lend, 50 * ONE), "when": {"when": "now"}},
    ])).await?;
    fund(&w, h, 0, 5 * NEAR).await?;
    step(&w, h, 0).await?.into_result()?;
    let r = step(&w, h, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")), "{:?}", r.logs());
    assert_eq!(balance(&w.usdc, w.vault.id()).await?, usdc, "no new loan");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": w.vault.id()})).await?.json()?;
    assert_eq!(acct["collateral"][0]["balance"], (20 * NEAR).to_string(), "all 20 wNEAR is collateral");

    // A repay the protocol refuses (bad message) must stop the recipe: no collateral released.
    let mut bad = repay_and_release(&w, &lend, NEAR / 3 * (SCALE / ONE));
    bad["calls"][0]["msg"] = json!("\"OnlyRepay\"");
    let wrap_before = balance(&w.wrap, w.vault.id()).await?;
    let b = save(&w, json!([{"input": w.usdc.id(), "action": bad, "when": {"when": "manual"}}])).await?;
    fund(&w, b, 0, 30 * ONE).await?;
    let r = w.alice.call(w.vault.id(), "step").args_json(json!({"flow_id": b, "node": 0})).max_gas().transact().await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")), "{:?}", r.logs());
    assert!(!r.logs().iter().any(|l| l.contains("decrease_collateral")), "collateral must not move: {:?}", r.logs());
    assert_eq!(balance(&w.wrap, w.vault.id()).await?, wrap_before);
    w.alice.call(w.vault.id(), "drain").args_json(json!({"flow_id": b})).transact().await?.into_result()?;

    // Repay 30 USDC and release 1/3 wNEAR per USDC: 10 wNEAR comes back to the vault.
    let wrap_before = balance(&w.wrap, w.vault.id()).await?;
    let back = save(&w, json!([{"input": w.usdc.id(), "action": repay_and_release(&w, &lend, NEAR / 3 * (SCALE / ONE)), "when": {"when": "manual"}}])).await?;
    fund(&w, back, 0, 30 * ONE).await?;
    let r = w.alice.call(w.vault.id(), "step").args_json(json!({"flow_id": back, "node": 0})).max_gas().transact().await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    let got = balance(&w.wrap, w.vault.id()).await? - wrap_before;
    println!("repaid 30 USDC, got {:.6} wNEAR back", got as f64 / NEAR as f64);
    assert!(got > 10 * NEAR - 1_000_000 && got <= 10 * NEAR, "{got}");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": w.vault.id()})).await?.json()?;
    println!("after repay: borrowed {}", acct["borrowed"]);
    Ok(())
}
