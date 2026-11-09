//! The relay vault factory: the relay (vault body) published once as a global
//! contract, one keyless vault per user, every vault obeying one fixed brain.
//! Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract, Worker};
use relayw::{view, x};
use serde_json::{json, Value};

/// A brain and a relay factory obeying it; no relay code published yet.
async fn factory(worker: &Worker<Sandbox>) -> anyhow::Result<(Account, Contract)> {
    let root = worker.root_account()?;
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(100)).transact().await?.into_result()?;
    let brain_acc = root.create_subaccount("brain").initial_balance(NearToken::from_near(15)).transact().await?.into_result()?;
    let brain = brain_acc.deploy(&wasm("brain")).await?.into_result()?;
    brain.call("new").args_json(json!({"factory": us.id()})).transact().await?.into_result()?;
    us.deploy(&wasm("factory_relay")).await?.into_result()?;
    us.call(us.id(), "new").args_json(json!({"owner": us.id(), "brain": brain.id()})).transact().await?.into_result()?;
    Ok((us, brain))
}

#[tokio::test]
async fn one_click_one_keyless_vault_per_user() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let (us, brain) = factory(&worker).await?;
    let fid = us.id();
    let (alice, bob) = (worker.dev_create_account().await?, worker.dev_create_account().await?);

    // No code yet: no vaults.
    let r = alice.call(fid, "create_vault").args_json(json!({"name": "alice", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("no vault code"), "{:?}", r.failures());

    // Publish the relay code once. Strangers can't.
    let code = wasm("relay");
    assert!(alice.call(fid, "publish").args(code.clone()).deposit(NearToken::from_near(2)).max_gas().transact().await?.is_failure());
    let before = us.view_account().await?.balance;
    let r = us.call(fid, "publish").args(code.clone()).deposit(NearToken::from_near(2)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    println!("publishing {:.1} KB of relay code: caller spent {:.3} NEAR",
        code.len() as f64 / 1024.0, (before.as_yoctonear() - us.view_account().await?.balance.as_yoctonear()) as f64 / 1e24);

    // Alice makes her vault in one call.
    let r = alice.call(fid, "create_vault").args_json(json!({"name": "alice", "keeper": bob.id()}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    println!("GAS create_vault: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    let vault: String = us.view(fid, "get_vault").args_json(json!({"owner": alice.id()})).await?.json()?;
    assert_eq!(vault, format!("alice.{fid}"));
    let vault_id: near_workspaces::AccountId = vault.parse()?;
    // Owner and keeper live in the vault's record in the brain.
    let rec = view(&brain, &vault_id, "get_vault", Value::Null).await?;
    assert_eq!(rec["owner"], alice.id().as_str());
    assert_eq!(rec["keeper"], bob.id().as_str());
    assert_eq!(worker.view(&vault_id, "brain_id").await?.json::<String>()?, brain.id().as_str());
    let details = worker.view_account(&vault_id).await?;
    println!("a vault: {} bytes of state, {:.3} NEAR", details.storage_usage, details.balance.as_yoctonear() as f64 / 1e24);
    assert!(details.storage_usage < 5_000, "the code is shared, not copied");
    assert!(worker.view_access_keys(&vault_id).await?.is_empty(), "no keys: nobody can redeploy it");
    // It works: Alice can save a flow in it, Bob (keeper) can't.
    let nodes = json!([{"input": "near", "action": {"label": "x", "calls": [{"call": "fn", "contract": "wrap.test.near", "method": "near_deposit",
        "args": "{}", "deposit": {"kind": "input"}, "tgas": 10}], "out": "wrap.test.near", "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    x(&alice, &vault_id, "trust", json!({"add": [{"contract": "wrap.test.near", "methods": ["near_deposit"]}], "remove": []})).await?.into_result()?;
    x(&alice, &vault_id, "save_flow", json!({"name": "first", "nodes": nodes})).await?.into_result()?;
    assert!(x(&bob, &vault_id, "save_flow", json!({"name": "x", "nodes": nodes})).await?.is_failure());

    let listed: Vec<String> = us.view(fid, "get_vaults").args_json(json!({"from_index": 0, "limit": 10})).await?.json()?;
    assert_eq!(listed, vec![vault.clone()], "keepers can find it");
    // One vault per person; a taken name refunds; tiny deposits and bad names are refused.
    assert!(alice.call(fid, "create_vault").args_json(json!({"name": "alice2", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.is_failure());
    let bob_before = bob.view_account().await?.balance.as_yoctonear();
    let r = bob.call(fid, "create_vault").args_json(json!({"name": "alice", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    let made: bool = r.json()?;
    assert!(!made, "the name is taken");
    worker.fast_forward(5).await?;
    let spent = bob_before - bob.view_account().await?.balance.as_yoctonear();
    assert!(spent < NearToken::from_millinear(10).as_yoctonear(), "Bob got his 1 NEAR back (spent only gas: {spent})");
    assert!(us.view(fid, "get_vault").args_json(json!({"owner": bob.id()})).await?.json::<Option<String>>()?.is_none(), "and may try again");
    assert!(bob.call(fid, "create_vault").args_json(json!({"name": "bob", "keeper": null}))
        .deposit(NearToken::from_millinear(99)).max_gas().transact().await?.is_failure(), "too little NEAR");
    let count: u32 = us.view(fid, "get_vault_count").await?.json()?;
    assert_eq!(count, 1, "the refused attempts aren't listed");
    assert!(bob.call(fid, "create_vault").args_json(json!({"name": "Bob.evil", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.is_failure(), "bad name");
    Ok(())
}

#[tokio::test]
async fn a_vault_costs_its_user_a_tenth_of_a_near() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let (us, brain) = factory(&worker).await?;
    us.call(us.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let carol = worker.dev_create_account().await?;
    assert!(carol.call(us.id(), "create_vault").args_json(json!({"name": "carol", "keeper": null}))
        .deposit(NearToken::from_millinear(99)).max_gas().transact().await?.is_failure(), "under 0.1 NEAR");
    let before = carol.view_account().await?.balance;
    let r = carol.call(us.id(), "create_vault").args_json(json!({"name": "carol", "keeper": null}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let vault: near_workspaces::AccountId = format!("carol.{}", us.id()).parse()?;
    // Unused prepaid gas comes back a few blocks later: count after the refunds.
    worker.fast_forward(5).await?;
    let spent = (before.as_yoctonear() - carol.view_account().await?.balance.as_yoctonear()) as f64 / 1e24;
    let v = worker.view_account(&vault).await?;
    println!("a relay vault for 0.1 NEAR (carol spent {spent:.4} incl. gas): it holds {:.4} NEAR, uses {} bytes", v.balance.as_yoctonear() as f64 / 1e24, v.storage_usage);
    // It works and its record can grow: trust 30 contracts. WHY the vault's own
    // bytes don't grow: its state lives in the brain, and the vault pays the brain
    // for its record from its own NEAR (get_storage: cost vs paid).
    let add: Vec<Value> = (0..30).map(|i| json!({"contract": format!("token-number-{i:02}.testnet"), "methods": ["ft_transfer_call_no", "deposit"]})).collect();
    x(&carol, &vault, "trust", json!({"add": add, "remove": []})).await?.into_result()?;
    let v = worker.view_account(&vault).await?;
    let (cost, paid): (String, String) = serde_json::from_value(view(&brain, &vault, "get_storage", Value::Null).await?)?;
    println!("after trusting 30 contracts: vault {} bytes, {:.4} NEAR; its record in the brain costs {:.4}, paid {:.4}", v.storage_usage, v.balance.as_yoctonear() as f64 / 1e24,
        cost.parse::<f64>()? / 1e24, paid.parse::<f64>()? / 1e24);
    assert!(paid.parse::<u128>()? >= cost.parse::<u128>()?, "the vault paid for its record");
    // The old full vault cost ~0.1 NEAR plus gas too (same 0.1 minimum); the relay's
    // creation round (relay new -> brain) burns more gas, still well under 0.11.
    assert!(spent < 0.11, "about a tenth of a NEAR: {spent}");
    Ok(())
}
