//! Bazaar ideas as flow nodes: a vault backs an idea, and a collector node
//! claims the delivered coin and sends it down the flow (here: sells half).
//! Build the wasm first: make build

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const SCALE: u128 = 1_000_000_000_000_000_000_000_000;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

struct World {
    worker: Worker<Sandbox>,
    alice: Account, // owns the vault
    keeper: Account,
    carol: Account, // sells the idea
    wnear: Contract,
    frog: Contract,
    dex: Contract,
    bazaar: Contract,
    vault: Contract,
}

async fn token(worker: &Worker<Sandbox>, owner: &Account, symbol: &str) -> anyhow::Result<Contract> {
    let t = worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": owner.id(), "total_supply": (1_000_000_000 * ONE).to_string(),
        "name": symbol, "symbol": symbol, "decimals": 18, "call_fee_bps": 0})).transact().await?.into_result()?;
    Ok(t)
}

async fn register(payer: &Account, token: &Contract, who: &AccountId) -> anyhow::Result<()> {
    payer.call(token.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn balance(token: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    let v: String = token.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json()?;
    Ok(v.parse()?)
}

async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.dev_create_account().await?;
    let (alice, keeper, carol) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    let wnear = token(&worker, &root, "wNEAR").await?;
    let frog = token(&worker, &carol, "FROG").await?; // Carol's coin-to-be
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": wnear.id()})).transact().await?.into_result()?;
    let bazaar = worker.dev_deploy(&wasm("bazaar")).await?;
    bazaar.call("new").args_json(json!({"treasury": root.id(), "review_ns": 60_000_000_000u64})).transact().await?.into_result()?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    for who in [bazaar.id(), vault.id(), dex.id(), carol.id(), alice.id()] {
        register(&root, &wnear, who).await?;
        register(&root, &frog, who).await?;
    }
    root.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000_000 * ONE).to_string()}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    root.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": vault.id(), "amount": (1_000 * ONE).to_string()}))
        .deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    for who in [vault.id(), carol.id()] {
        root.call(bazaar.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_near(1)).transact().await?.into_result()?;
    }
    dex.call("set_rate").args_json(json!({"token_in": frog.id(), "token_out": wnear.id(), "rate": (SCALE / 1_000).to_string()})).transact().await?.into_result()?;
    alice.call(vault.id(), "trust").args_json(json!({"add": [
        {"contract": wnear.id()}, {"contract": frog.id()}, {"contract": dex.id()},
        {"contract": bazaar.id(), "methods": ["claim", "reject"]}], "remove": []})).transact().await?.into_result()?;
    Ok(World { worker, alice, keeper, carol, wnear, frog, dex, bazaar, vault })
}

/// Carol lists an idea: 100 shares for 1,000 wNEAR, pledging 1M FROG within 10 days.
async fn idea(w: &World, title: &str) -> anyhow::Result<u64> {
    let now: u64 = w.worker.view_block().await?.timestamp();
    let id: u64 = w.carol.call(w.bazaar.id(), "sell_idea").args_json(json!({"terms": {"sale": "listing", "ask": (1_000 * ONE).to_string()},
        "shares": "100", "pledge": {"amount": (1_000_000 * ONE).to_string(), "deliver_by_ns": now + 240 * 3_600_000_000_000u64},
        "pay": w.wnear.id(), "lore": {"title": title}})).max_gas().transact().await?.into_result()?.json()?;
    Ok(id)
}

/// Back idea `lot` with wNEAR; then claim its coin (or refund) every minute and sell half the coin.
fn idea_flow(w: &World, lot: u64) -> Value {
    json!([
        {"input": w.wnear.id(), "when": {"when": "now"}, "action": {"label": "Back an idea", "out": null, "min_out_per_in": null, "price": null,
            "calls": [{"call": "ft", "receiver": w.bazaar.id(), "msg": format!(r#"{{"action":"buy","lot":{lot}}}"#), "tgas": 80}]}},
        {"input": "none", "when": {"when": "every", "every_ns": 60000000000u64, "chunk": "1"}, "next": [{"to": 2, "bps": 5000}],
         "action": {"label": "Claim the coin", "out": w.frog.id(), "min_out_per_in": null, "price": null,
            "calls": [{"call": "fn", "contract": w.bazaar.id(), "method": "claim", "args": format!(r#"{{"lot_id":{lot},"account":"{{vault}}"}}"#),
                       "deposit": {"kind": "none"}, "tgas": 60}]}},
        {"input": w.frog.id(), "when": {"when": "now"}, "action": {"label": "Sell half", "out": w.wnear.id(), "min_out_per_in": null, "price": null,
            "calls": [{"call": "ft", "receiver": w.dex.id(), "msg": format!(r#"{{"token_out":"{}","min_out":"0"}}"#, w.wnear.id()), "tgas": 50}]}},
    ])
}

async fn step(w: &World, flow: u32, node: u16) -> anyhow::Result<ExecutionFinalResult> {
    let r = w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": flow, "node": node})).max_gas().transact().await?;
    println!("GAS step {node}: {:.1} Tgas {}", r.total_gas_burnt.as_gas() as f64 / 1e12,
        r.logs().iter().filter(|l| l.contains("flows")).cloned().collect::<Vec<_>>().join(" "));
    Ok(r)
}

/// Run `finalize` until the (1-minute) review is over.
async fn finalize(w: &World, lot: u64) -> anyhow::Result<()> {
    for _ in 0..80 {
        if w.carol.call(w.bazaar.id(), "finalize").args_json(json!({"lot_id": lot})).max_gas().transact().await?.is_success() {
            return Ok(());
        }
        w.worker.fast_forward(50).await?;
    }
    anyhow::bail!("review never ended")
}

#[tokio::test]
async fn a_flow_backs_an_idea_claims_the_coin_and_sells_half() -> anyhow::Result<()> {
    let w = setup().await?;
    let lot = idea(&w, "FROG").await?;
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "back FROG", "nodes": idea_flow(&w, lot)}))
        .max_gas().transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": (300 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
    assert!(step(&w, f, 0).await?.is_success());
    let stake: Value = w.bazaar.view("get_stake").args_json(json!({"lot_id": lot, "account_id": w.vault.id()})).await?.json()?;
    assert_eq!(stake["shares"], "30", "the vault backed 30 shares");
    // Nothing to claim yet: the collector runs, finds nothing, and isn't paused for it.
    let r = step(&w, f, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")) && !r.logs().iter().any(|l| l.contains(r#""paused":true"#)));
    // Carol delivers; the review passes; the collector claims 300k FROG and sends half on.
    w.carol.call(w.frog.id(), "ft_transfer_call").args_json(json!({"receiver_id": w.bazaar.id(), "amount": (300_000 * ONE).to_string(),
        "msg": format!(r#"{{"action":"deliver","lot":{lot}}}"#)})).deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    finalize(&w, lot).await?;
    for _ in 0..10 {
        if step(&w, f, 1).await?.logs().iter().any(|l| l.contains(r#""event":"step""#)) {
            break;
        }
        w.worker.fast_forward(70).await?; // the collector's schedule: once a minute
    }
    assert_eq!(balance(&w.frog, w.vault.id()).await?, 300_000 * ONE, "the delivered coin is in the vault");
    let s: Value = w.vault.view("get_flow").args_json(json!({"flow_id": f})).await?.json()?;
    assert_eq!(s["ledger"]["nodes"][2]["inbox"], (150_000 * ONE).to_string(), "half routed to the sell node");
    assert!(step(&w, f, 2).await?.is_success());
    assert_eq!(balance(&w.frog, w.vault.id()).await?, 150_000 * ONE);
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 700 * ONE + 150 * ONE, "150k FROG sold at 0.001");
    Ok(())
}

#[tokio::test]
async fn the_owner_rejects_a_junk_delivery_and_the_flow_collects_the_refund() -> anyhow::Result<()> {
    let w = setup().await?;
    let lot = idea(&w, "Totally real").await?;
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "back it", "nodes": idea_flow(&w, lot)}))
        .max_gas().transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": (300 * ONE).to_string()})).max_gas().transact().await?.into_result()?;
    step(&w, f, 0).await?.into_result()?;
    w.carol.call(w.frog.id(), "ft_transfer_call").args_json(json!({"receiver_id": w.bazaar.id(), "amount": (300_000 * ONE).to_string(),
        "msg": format!(r#"{{"action":"deliver","lot":{lot}}}"#)})).deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    // Alice looks at the delivery, doesn't like it, and rejects from her vault (a one-off action).
    let r = w.alice.call(w.vault.id(), "act").args_json(json!({"target": w.bazaar.id(), "method": "reject", "json_args": format!(r#"{{"lot_id":{lot}}}"#), "one_yocto": false}))
        .max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    // The keeper can't make one-off calls, and nobody can "act" with a fund-moving method.
    assert!(w.keeper.call(w.vault.id(), "act").args_json(json!({"target": w.bazaar.id(), "method": "reject", "json_args": "{}", "one_yocto": false}))
        .max_gas().transact().await?.is_failure());
    assert!(w.alice.call(w.vault.id(), "act").args_json(json!({"target": w.wnear.id(), "method": "ft_transfer", "json_args": "{}", "one_yocto": true}))
        .max_gas().transact().await?.is_failure());
    finalize(&w, lot).await?;
    for _ in 0..10 {
        let r = step(&w, f, 1).await?;
        if r.logs().iter().any(|l| l.contains("step")) && balance(&w.wnear, w.vault.id()).await? == 1_000 * ONE {
            break;
        }
        w.worker.fast_forward(70).await?;
    }
    assert_eq!(balance(&w.wnear, w.vault.id()).await?, 1_000 * ONE, "the 300 wNEAR came back to the vault");
    assert_eq!(balance(&w.frog, w.vault.id()).await?, 0, "no junk");
    let _ = &w.dex;
    Ok(())
}
