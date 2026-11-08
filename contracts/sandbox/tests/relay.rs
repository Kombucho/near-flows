//! The body/brain split: each user's vault is the 9 KB relay (a global
//! contract), all rules and state live in one shared brain. Same swap flow as
//! the old full vault's, run through relay + brain, plus the attacks on the relay
//! and the measured costs. Build the wasm first: make build

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000; // 1 token (18 decimals)
const SCALE: u128 = 1_000_000_000_000_000_000_000_000;
const TGAS: u64 = 1_000_000_000_000;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

fn near(yocto: u128) -> f64 {
    yocto as f64 / 1e24
}

struct World {
    worker: Worker<Sandbox>,
    us: Account,
    brain: Contract,
    alice: Account,
    keeper: Account,
    mallory: Account,
    wnear: Contract,
    usdc: Contract,
    dex: Contract,
    vault: AccountId,
    /// What publishing the relay cost us (NEAR, incl. gas).
    publish_cost: f64,
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

/// A request to a relay vault: its args go to the brain untouched.
async fn x(who: &Account, vault: &AccountId, req: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(req.to_string().into_bytes()).max_gas().transact().await?)
}

fn fails_with(r: &ExecutionFinalResult, why: &str) -> bool {
    format!("{:?}", r.failures()).contains(why)
}

/// Brain + relay factory + relay published; Alice's vault holds 1,000 wNEAR.
async fn setup() -> anyhow::Result<World> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(50)).transact().await?.into_result()?;
    let brain_acc = root.create_subaccount("brain").initial_balance(NearToken::from_near(10)).transact().await?.into_result()?;
    let brain = brain_acc.deploy(&wasm("brain")).await?.into_result()?;
    brain.call("new").args_json(json!({"factory": us.id()})).transact().await?.into_result()?;
    let factory = us.deploy(&wasm("factory_relay")).await?.into_result()?;
    factory.call("new").args_json(json!({"owner": us.id(), "brain": brain.id()})).transact().await?.into_result()?;
    let before = us.view_account().await?.balance.as_yoctonear();
    us.call(us.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    worker.fast_forward(3).await?;
    let publish_cost = near(before - us.view_account().await?.balance.as_yoctonear());

    let (alice, keeper, mallory) = (worker.dev_create_account().await?, worker.dev_create_account().await?, worker.dev_create_account().await?);
    alice.call(us.id(), "create_vault").args_json(json!({"name": "alice", "keeper": keeper.id()}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;
    let vault: AccountId = format!("alice.{}", us.id()).parse()?;

    let minter = worker.dev_create_account().await?;
    let (wnear, usdc, lst) = (token(&worker, &minter, "wNEAR").await?, token(&worker, &minter, "USDC").await?, token(&worker, &minter, "LST").await?);
    let dex = worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [alice.id(), dex.id(), &vault, mallory.id()] {
            register(&minter, t, who).await?;
        }
        give(&minter, t, dex.id(), 100_000_000 * ONE).await?;
    }
    give(&minter, &wnear, alice.id(), 10_000 * ONE).await?;
    give(&alice, &wnear, &vault, 1_000 * ONE).await?;
    for (a, b, r) in [(&wnear, &usdc, 2 * SCALE), (&usdc, &wnear, SCALE / 2)] {
        alice.call(dex.id(), "set_rate").args_json(json!({"token_in": a.id(), "token_out": b.id(), "rate": r.to_string()})).transact().await?.into_result()?;
    }
    x(&alice, &vault, json!({"trust": {"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id(), "methods": ["get_return"]}], "remove": []}}))
        .await?.into_result()?;
    Ok(World { worker, us, brain, alice, keeper, mallory, wnear, usdc, dex, vault, publish_cost })
}

fn swap(w: &World, to: &Contract, min_out_per_in: Option<u128>) -> Value {
    json!({"label": "Swap", "calls": [{"call": "ft", "receiver": w.dex.id(), "tgas": 50,
            "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, to.id())}],
           "out": to.id(), "min_out_per_in": min_out_per_in.map(|m| m.to_string()), "price": null})
}

async fn flow_state(w: &World, flow: u32) -> anyhow::Result<Value> {
    Ok(w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": flow})).await?.json()?)
}

fn report(what: &str, r: &ExecutionFinalResult) {
    println!("GAS {what}: {:.1} Tgas burnt over {} receipts", r.total_gas_burnt.as_gas() as f64 / 1e12, r.receipt_outcomes().len());
    for o in r.receipt_outcomes() {
        if what == "relay step" {
            println!("  {:>5.1} Tg  {}", o.gas_burnt.as_gas() as f64 / 1e12, o.executor_id);
        }
        if o.is_failure() {
            println!("  {} {:?}", o.executor_id, o.clone().into_result().err());
        }
    }
}

#[tokio::test]
async fn a_swap_flow_runs_end_to_end_through_relay_and_brain() -> anyhow::Result<()> {
    let w = setup().await?;
    assert_eq!(w.worker.view(&w.vault, "brain_id").await?.json::<String>()?, w.brain.id().as_str());
    let rec: Value = w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json()?;
    assert_eq!(rec["owner"], w.alice.id().as_str());
    assert_eq!(rec["keeper"], w.keeper.id().as_str());
    assert!(w.worker.view_access_keys(&w.vault).await?.is_empty(), "keyless: nobody can redeploy it");

    // 100 wNEAR -> USDC; 60% of the USDC goes on to a swap back to wNEAR, 40% is free.
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(19 * SCALE / 10)), "when": {"when": "now"}, "next": [{"to": 1, "bps": 6000}]});
    let n1 = json!({"input": w.usdc.id(), "action": swap(&w, &w.wnear, None), "when": {"when": "now"}});
    let r = x(&w.alice, &w.vault, json!({"save_flow": {"name": "test", "nodes": [n0, n1]}})).await?;
    report("save_flow", &r);
    let f: u32 = r.json()?;
    let r = x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (100 * ONE).to_string()}})).await?;
    report("fund", &r);
    r.into_result()?;
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?;
    report("step", &r);
    assert!(r.is_success(), "{:?}", r.failures());
    let out: Value = r.json()?;
    assert_eq!(out["out"], (200 * ONE).to_string(), "the step's result comes back to the keeper: {out}");
    let s = flow_state(&w, f).await?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (120 * ONE).to_string(), "60% of 200 USDC routed on");
    assert_eq!(balance(&w.usdc, &w.vault).await?, 200 * ONE);
    let alloc: String = w.brain.view("get_allocated").args_json(json!({"vault": w.vault, "token": w.usdc.id()})).await?.json()?;
    assert_eq!(alloc, (120 * ONE).to_string(), "the other 80 USDC is free");
    // Second hop: 120 USDC -> 60 wNEAR.
    assert!(x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 1}})).await?.is_success());
    assert_eq!(balance(&w.wnear, &w.vault).await?, 960 * ONE);

    // Withdraw: never what a flow holds, the rest to the owner only.
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (900 * ONE).to_string()}})).await?.into_result()?;
    let too_much = x(&w.alice, &w.vault, json!({"withdraw": {"token": w.wnear.id(), "amount": (61 * ONE).to_string()}})).await?;
    assert!(fails_with(&too_much, "not enough free balance"), "900 of 960 wNEAR is in the flow");
    let before = balance(&w.wnear, w.alice.id()).await?;
    let r = x(&w.alice, &w.vault, json!({"withdraw": {"token": w.wnear.id(), "amount": (60 * ONE).to_string()}})).await?;
    report("withdraw token", &r);
    r.into_result()?;
    assert_eq!(balance(&w.wnear, w.alice.id()).await? - before, 60 * ONE);
    let before = w.alice.view_account().await?.balance.as_yoctonear();
    x(&w.alice, &w.vault, json!({"withdraw": {"token": "near", "amount": (SCALE / 2).to_string()}})).await?.into_result()?;
    assert!(w.alice.view_account().await?.balance.as_yoctonear() > before + SCALE / 2 - SCALE / 100, "0.5 NEAR back (less gas)");
    assert!(x(&w.alice, &w.vault, json!({"withdraw": {"token": "near", "amount": (SCALE / 2).to_string()}})).await?.is_failure(), "only ~0.4 NEAR left free");

    // Tokens sent with ft_transfer_call are kept (the relay answers the hook itself).
    w.alice.call(w.wnear.id(), "ft_transfer_call").args_json(json!({"receiver_id": w.vault, "amount": ONE.to_string(), "msg": ""}))
        .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await?.into_result()?;
    assert_eq!(balance(&w.wnear, &w.vault).await?, 901 * ONE);

    // A refused swap (minimum not met) keeps the money and pauses nothing yet.
    let g = x(&w.alice, &w.vault, json!({"save_flow": {"name": "greedy", "nodes": [
        {"input": w.wnear.id(), "action": swap(&w, &w.usdc, Some(3 * SCALE)), "when": {"when": "now"}}]}})).await?.json::<u32>()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": g, "node": 0, "amount": ONE.to_string()}})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": g, "node": 0}})).await?;
    assert!(r.logs().join("\n").contains("step_failed"), "{:?}", r.logs());
    assert_eq!(balance(&w.wnear, &w.vault).await?, 901 * ONE, "nothing lost");
    assert_eq!(flow_state(&w, g).await?["ledger"]["nodes"][0]["inbox"], ONE.to_string());
    let (cost, paid): (String, String) = w.brain.view("get_storage").args_json(json!({"vault": w.vault})).await?.json()?;
    println!("Alice's record in the brain: costs {:.4} NEAR, she paid {:.4}", near(cost.parse()?), near(paid.parse()?));
    assert!(paid.parse::<u128>()? >= cost.parse::<u128>()?, "the vault pays for its own record");
    Ok(())
}

#[tokio::test]
async fn a_stranger_cant_make_the_relay_do_anything() -> anyhow::Result<()> {
    let w = setup().await?;
    let n0 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}});
    let n1 = json!({"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "manual"}});
    let f: u32 = x(&w.alice, &w.vault, json!({"save_flow": {"name": "t", "nodes": [n0, n1]}})).await?.json()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()}})).await?.into_result()?;
    x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 1, "amount": (10 * ONE).to_string()}})).await?.into_result()?;
    let near_before = w.worker.view_account(&w.vault).await?.balance.as_yoctonear();

    // Every request is the brain's to refuse, with the caller's real name.
    let m = &w.mallory;
    assert!(fails_with(&x(m, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?, "owner or keeper only"));
    assert!(fails_with(&x(m, &w.vault, json!({"withdraw": {"token": w.wnear.id(), "amount": "1"}})).await?, "owner only"));
    assert!(fails_with(&x(m, &w.vault, json!({"withdraw": {"token": "near", "amount": "1"}})).await?, "owner only"));
    assert!(fails_with(&x(m, &w.vault, json!({"set_keeper": {"keeper": m.id()}})).await?, "owner only"));
    assert!(fails_with(&x(m, &w.vault, json!({"trust": {"add": [{"contract": m.id()}], "remove": []}})).await?, "owner only"));
    assert!(fails_with(&x(m, &w.vault, json!({"init": {"owner": m.id(), "keeper": null}})).await?, "vaults are made by the factory"));
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"withdraw": {"token": w.wnear.id(), "amount": "1"}})).await?, "owner only"));
    assert!(fails_with(&x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 1}})).await?, "OwnerOnly"), "manual nodes: owner only");
    assert!(x(&w.alice, &w.vault, json!({"trust": {"add": [{"contract": w.dex.id(), "methods": ["ft_transfer"]}], "remove": []}})).await?.is_failure(),
        "even the owner can't let recipes move funds");
    assert!(x(&w.alice, &w.vault, json!({"trust": {"add": [{"contract": w.brain.id(), "methods": ["get_vault"]}], "remove": []}})).await?.is_failure(),
        "nor talk to the brain as the vault");

    // The relay's other doors: its callback, its init.
    let r = m.call(&w.vault, "cb").args(vec![1, 1, 1, 5]).max_gas().transact().await?;
    assert!(fails_with(&r, "only the vault's own callback"), "{:?}", r.failures());
    let mut init = vec![w.mallory.id().len() as u8];
    init.extend(w.mallory.id().as_bytes());
    let r = m.call(&w.vault, "new").args(init).max_gas().transact().await?;
    assert!(fails_with(&r, "already initialised"), "the brain is fixed for good");
    // The brain's doors, called directly: only vaults talk to it.
    for method in ["handle", "on_vault", "on_read"] {
        let mut hdr = vec![1u8];
        hdr.extend([0u8; 48]);
        hdr.push(0);
        if method == "on_vault" {
            hdr.extend([0u8; 18]); // a well-formed Next::Call {id: 0, i: 0, tgas: 0}: refused for who sends it, not its shape
        }
        let r = m.call(w.brain.id(), method).args(hdr).max_gas().transact().await?;
        let why = [("handle", "a request names its sender"), ("on_vault", "not a vault"), ("on_read", "is private")].iter().find(|(m, _)| *m == method).unwrap().1;
        assert!(fails_with(&r, why), "{method} from a stranger: {:?}", r.failures());
    }
    assert!(fails_with(&m.call(w.brain.id(), "pay").deposit(NearToken::from_millinear(1)).transact().await?, "not a vault"));

    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "nothing moved");
    assert!(w.worker.view_account(&w.vault).await?.balance.as_yoctonear() >= near_before, "not even NEAR");
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?;
    assert!(r.is_success(), "and the keeper still runs its step: {:?}", r.failures());
    Ok(())
}

async fn forge(fake: &Contract, a: Vec<u8>) -> anyhow::Result<()> {
    fake.call("set_answer").args(a).transact().await?.into_result()?;
    Ok(())
}

/// The answer bytes a brain would send (format in relay/src/lib.rs).
fn answer(calls: &[(&str, &str, &[u8])]) -> Vec<u8> {
    let mut a = vec![1u8, calls.len() as u8];
    for (to, method, args) in calls {
        a.push(0);
        a.push(to.len() as u8);
        a.extend(to.as_bytes());
        a.push(method.len() as u8);
        a.extend(method.as_bytes());
        a.extend((args.len() as u32).to_le_bytes());
        a.extend(*args);
        a.extend(0u128.to_le_bytes());
        a.extend((200 * TGAS).to_le_bytes());
    }
    a.push(0);
    a.extend(0u32.to_le_bytes());
    a
}

#[tokio::test]
async fn a_forged_brain_answer_is_refused() -> anyhow::Result<()> {
    let w = setup().await?;
    // Mallory runs her own relay obeying her own brain, which answers whatever she likes.
    let fake = w.worker.dev_deploy(&wasm("mock_brain")).await?;
    fake.call("new").transact().await?.into_result()?;
    let evil = w.mallory.create_subaccount("evil").initial_balance(NearToken::from_near(2)).transact().await?.into_result()?;
    let evil = evil.deploy(&wasm("relay")).await?.into_result()?;
    let mut init = vec![fake.id().len() as u8];
    init.extend(fake.id().as_bytes());
    init.push(w.mallory.id().len() as u8);
    init.extend(w.mallory.id().as_bytes());
    let _ = w.mallory.call(evil.id(), "new").args(init).max_gas().transact().await?;

    // 1. "Brain answer" delivered to Alice's vault's cb by another contract: refused (wrong predecessor).
    forge(&fake, answer(&[(w.vault.as_str(), "cb", b"")])).await?;
    let r = w.mallory.call(evil.id(), "x").args(b"{}".to_vec()).max_gas().transact().await?;
    assert!(fails_with(&r, "only the vault's own callback"), "{:?}", r.failures());
    // 2. Asking Alice's vault through x: the brain sees Mallory's relay as the sender, refuses.
    let req = json!({"withdraw": {"token": w.wnear.id(), "amount": (1_000 * ONE).to_string()}}).to_string();
    forge(&fake, answer(&[(w.vault.as_str(), "x", req.as_bytes())])).await?;
    let r = w.mallory.call(evil.id(), "x").args(b"{}".to_vec()).max_gas().transact().await?;
    assert!(fails_with(&r, "owner only"), "{:?}", r.failures());
    // 3. Even from its own brain, a relay never targets itself (no self-made cb).
    forge(&fake, answer(&[(evil.id().as_str(), "cb", b"")])).await?;
    let r = w.mallory.call(evil.id(), "x").args(b"{}".to_vec()).max_gas().transact().await?;
    assert!(fails_with(&r, "an action can't target the vault itself"), "{:?}", r.failures());
    // 4. Garbage answers are refused whole.
    forge(&fake, vec![1, 1, 9]).await?;
    assert!(fails_with(&w.mallory.call(evil.id(), "x").args(b"{}".to_vec()).max_gas().transact().await?, "bad action"));
    // 5. A relay that's not the factory's can't get a record in the real brain.
    let evil2 = w.mallory.create_subaccount("evil2").initial_balance(NearToken::from_near(2)).transact().await?.into_result()?;
    let evil2 = evil2.deploy(&wasm("relay")).await?.into_result()?;
    let mut init = vec![w.brain.id().len() as u8];
    init.extend(w.brain.id().as_bytes());
    init.push(w.mallory.id().len() as u8);
    init.extend(w.mallory.id().as_bytes());
    init.extend(json!({"init": {"owner": w.mallory.id(), "keeper": null}}).to_string().as_bytes());
    let r = w.mallory.call(evil2.id(), "new").args(init).max_gas().transact().await?;
    assert!(fails_with(&r, "vaults are made by the factory"), "{:?}", r.failures());
    assert!(w.brain.view("get_vault").args_json(json!({"vault": evil2.id()})).await?.json::<Option<Value>>()?.is_none());
    assert!(w.worker.view_account(evil2.id()).await.is_err(), "a relay the brain refused deletes itself, its NEAR to the payer");

    assert_eq!(balance(&w.wnear, &w.vault).await?, 1_000 * ONE, "Alice's vault untouched");
    // And with an honest answer, a relay does exactly what it says (here: pay Alice 1 NEAR).
    let mut a = vec![1u8, 1, 1, w.alice.id().len() as u8];
    a.extend(w.alice.id().as_bytes());
    a.extend(SCALE.to_le_bytes());
    a.push(0);
    a.extend(0u32.to_le_bytes());
    forge(&fake, a).await?;
    let before = w.alice.view_account().await?.balance.as_yoctonear();
    w.mallory.call(evil.id(), "x").args(b"{}".to_vec()).max_gas().transact().await?.into_result()?;
    assert_eq!(w.alice.view_account().await?.balance.as_yoctonear() - before, SCALE);
    Ok(())
}

#[tokio::test]
async fn the_split_costs_about_one_near_to_publish_and_this_much_gas_per_step() -> anyhow::Result<()> {
    let w = setup().await?;
    let relay_kb = wasm("relay").len() as f64 / 1024.0;
    let brain = w.worker.view_account(w.brain.id()).await?;
    println!("relay wasm: {relay_kb:.1} KB; publishing it as a global contract cost {:.3} NEAR (incl. gas)", w.publish_cost);
    println!("brain wasm: {:.1} KB; deployed, it stakes {:.2} NEAR of storage ({} bytes incl. state; refundable)",
        wasm("brain").len() as f64 / 1024.0, brain.storage_usage as f64 * 1e19 / 1e24, brain.storage_usage);
    assert!(w.publish_cost < 2.0, "publishing the body costs ~1 NEAR, not 59");

    // A vault for 0.1 NEAR still works (its record in the brain is paid from it).
    let carol = w.worker.dev_create_account().await?;
    let before = carol.view_account().await?.balance.as_yoctonear();
    let r = carol.call(w.us.id(), "create_vault").args_json(json!({"name": "carol", "keeper": null}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    report("create_vault", &r);
    r.into_result()?;
    w.worker.fast_forward(5).await?;
    let cv: AccountId = format!("carol.{}", w.us.id()).parse()?;
    let acc = w.worker.view_account(&cv).await?;
    println!("a relay vault for 0.1 NEAR: carol spent {:.4} incl. gas; vault holds {:.4} NEAR, uses {} bytes",
        near(before - carol.view_account().await?.balance.as_yoctonear()), near(acc.balance.as_yoctonear()), acc.storage_usage);
    let add: Vec<Value> = (0..30).map(|i| json!({"contract": format!("token-number-{i:02}.testnet"), "methods": ["deposit"]})).collect();
    x(&carol, &cv, json!({"trust": {"add": add, "remove": []}})).await?.into_result()?;
    let (cost, paid): (String, String) = w.brain.view("get_storage").args_json(json!({"vault": cv})).await?.json()?;
    println!("after trusting 30 contracts: record costs {:.4} NEAR in the brain, paid {:.4}; vault holds {:.4}",
        near(cost.parse()?), near(paid.parse()?), near(w.worker.view_account(&cv).await?.balance.as_yoctonear()));
    assert!(paid.parse::<u128>()? >= cost.parse::<u128>()?);

    // Gas per request of a one-swap flow on relay + brain.
    // WHY no comparison any more: the full vault (contracts/vault) is gone. Its
    // last measured figures (2026-11-06, same flow, same sandbox) for the record:
    //   full vault wasm 735.1 KB (73.5 NEAR to publish at 0.1 NEAR/KB);
    //   save_flow 2.4 Tg vs 8.7 relay (3.66x), fund 6.8 vs 10.5 (1.53x), step 29.0 vs 32.5 (1.12x).
    let node = json!([{"input": w.wnear.id(), "action": swap(&w, &w.usdc, None), "when": {"when": "now"}}]);
    let r = x(&w.alice, &w.vault, json!({"save_flow": {"name": "g", "nodes": node}})).await?;
    let mut split = vec![("save_flow", r.total_gas_burnt.as_gas())];
    let f: u32 = r.json()?;
    let r = x(&w.alice, &w.vault, json!({"fund": {"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()}})).await?;
    split.push(("fund", r.into_result()?.total_gas_burnt.as_gas()));
    let r = x(&w.keeper, &w.vault, json!({"step": {"flow_id": f, "node": 0}})).await?;
    report("relay step", &r);
    split.push(("step", r.into_result()?.total_gas_burnt.as_gas()));
    assert_eq!(balance(&w.usdc, &w.vault).await?, 20 * ONE, "the relay's swap really ran");
    for (what, g) in split {
        println!("{what:<10} {:>7.1} Tgas on relay+brain", g as f64 / 1e12);
    }
    Ok(())
}
