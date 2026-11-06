//! Vault factory: the vault code published once (global contract), one keyless
//! vault per user. Build the wasm first: make build

use near_workspaces::types::NearToken;
use serde_json::json;

fn wasm(name: &str) -> Vec<u8> {
    let path = format!("{}/../target/near/{name}/{name}.wasm", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing {path}: run `make build` first"))
}

#[tokio::test]
async fn one_click_one_keyless_vault_per_user() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(100)).transact().await?.into_result()?;
    let factory = us.deploy(&wasm("factory")).await?.into_result()?;
    factory.call("new").args_json(json!({"owner": us.id()})).transact().await?.into_result()?;
    let (alice, bob) = (worker.dev_create_account().await?, worker.dev_create_account().await?);

    // No code yet: no vaults.
    let r = alice.call(factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    assert!(format!("{:?}", r.failures()).contains("no vault code"));

    // Publish the vault code once. Strangers can't.
    let code = wasm("vault");
    assert!(alice.call(factory.id(), "publish").args(code.clone()).deposit(NearToken::from_near(40)).max_gas().transact().await?.is_failure());
    let before = us.view_account().await?.balance;
    let r = us.call(factory.id(), "publish").args(code.clone()).deposit(NearToken::from_near(40)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let after_factory = factory.view_account().await?;
    println!("publishing {} KB of vault code: deposit 40 NEAR, factory now holds {:.2} NEAR (storage {} bytes); caller spent {:.3} NEAR",
        code.len() / 1024, after_factory.balance.as_yoctonear() as f64 / 1e24, after_factory.storage_usage,
        (before.as_yoctonear() - us.view_account().await?.balance.as_yoctonear()) as f64 / 1e24);

    // Alice makes her vault in one call.
    let r = alice.call(factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": bob.id()}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    println!("GAS create_vault: {:.1} Tgas", r.total_gas_burnt.as_gas() as f64 / 1e12);
    assert!(r.is_success(), "{:?}", r.failures());
    let vault: String = factory.view("get_vault").args_json(json!({"owner": alice.id()})).await?.json()?;
    assert_eq!(vault, format!("alice.{}", factory.id()));
    let vault_id: near_workspaces::AccountId = vault.parse()?;
    let owner: String = worker.view(&vault_id, "get_owner").await?.json()?;
    assert_eq!(owner, alice.id().as_str());
    let keeper: Option<String> = worker.view(&vault_id, "get_keeper").await?.json()?;
    assert_eq!(keeper.as_deref(), Some(bob.id().as_str()));
    let details = worker.view_account(&vault_id).await?;
    println!("a vault: {} bytes of state, {:.3} NEAR", details.storage_usage, details.balance.as_yoctonear() as f64 / 1e24);
    assert!(details.storage_usage < 5_000, "the code is shared, not copied");
    assert!(worker.view_access_keys(&vault_id).await?.is_empty(), "no keys: nobody can redeploy it");
    // It works: Alice can save a flow in it, Bob (keeper) can't.
    let nodes = json!([{"input": "near", "action": {"label": "x", "calls": [{"call": "fn", "contract": "wrap.test.near", "method": "near_deposit",
        "args": "{}", "deposit": {"kind": "input"}, "tgas": 10}], "out": "wrap.test.near", "min_out_per_in": null, "price": null}, "when": {"when": "now"}}]);
    alice.call(&vault_id, "trust").args_json(json!({"add": [{"contract": "wrap.test.near", "methods": ["near_deposit"]}], "remove": []})).transact().await?.into_result()?;
    alice.call(&vault_id, "save_flow").args_json(json!({"name": "first", "nodes": nodes})).max_gas().transact().await?.into_result()?;
    assert!(bob.call(&vault_id, "save_flow").args_json(json!({"name": "x", "nodes": nodes})).max_gas().transact().await?.is_failure());

    let listed: Vec<String> = factory.view("get_vaults").args_json(json!({"from_index": 0, "limit": 10})).await?.json()?;
    assert_eq!(listed, vec![vault.clone()], "keepers can find it");
    // One vault per person; a taken name refunds; tiny deposits and bad names are refused.
    assert!(alice.call(factory.id(), "create_vault").args_json(json!({"name": "alice2", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.is_failure());
    let bob_before = bob.view_account().await?.balance.as_yoctonear();
    let r = bob.call(factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    let made: bool = r.json()?;
    assert!(!made, "the name is taken");
    let spent = bob_before - bob.view_account().await?.balance.as_yoctonear();
    assert!(spent < NearToken::from_millinear(10).as_yoctonear(), "Bob got his 1 NEAR back (spent only gas: {spent})");
    assert!(factory.view("get_vault").args_json(json!({"owner": bob.id()})).await?.json::<Option<String>>()?.is_none(), "and may try again");
    assert!(bob.call(factory.id(), "create_vault").args_json(json!({"name": "bob", "keeper": null}))
        .deposit(NearToken::from_millinear(99)).max_gas().transact().await?.is_failure(), "too little NEAR");
    let count: u32 = factory.view("get_vault_count").await?.json()?;
    assert_eq!(count, 1, "the refused attempts aren't listed");
    assert!(bob.call(factory.id(), "create_vault").args_json(json!({"name": "Bob.evil", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?.is_failure(), "bad name");
    Ok(())
}

#[tokio::test]
async fn a_vault_costs_its_user_a_tenth_of_a_near() -> anyhow::Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let us = root.create_subaccount("flows").initial_balance(NearToken::from_near(100)).transact().await?.into_result()?;
    let factory = us.deploy(&wasm("factory")).await?.into_result()?;
    factory.call("new").args_json(json!({"owner": us.id()})).transact().await?.into_result()?;
    us.call(factory.id(), "publish").args(wasm("vault")).deposit(NearToken::from_near(1)).max_gas().transact().await?.into_result()?;
    let carol = worker.dev_create_account().await?;
    assert!(carol.call(factory.id(), "create_vault").args_json(json!({"name": "carol", "keeper": null}))
        .deposit(NearToken::from_millinear(99)).max_gas().transact().await?.is_failure(), "under 0.1 NEAR");
    let before = carol.view_account().await?.balance;
    let r = carol.call(factory.id(), "create_vault").args_json(json!({"name": "carol", "keeper": null}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let vault: near_workspaces::AccountId = format!("carol.{}", factory.id()).parse()?;
    // Unused prepaid gas comes back a few blocks later: count after the refunds.
    worker.fast_forward(5).await?;
    let spent = (before.as_yoctonear() - carol.view_account().await?.balance.as_yoctonear()) as f64 / 1e24;
    let v = worker.view_account(&vault).await?;
    println!("a vault for 0.1 NEAR (carol spent {spent:.4} incl. gas): it holds {:.4} NEAR, uses {} bytes", v.balance.as_yoctonear() as f64 / 1e24, v.storage_usage);
    // It works and its state can grow: trust 30 contracts, ~2 KB more state.
    let add: Vec<serde_json::Value> = (0..30).map(|i| json!({"contract": format!("token-number-{i:02}.testnet"), "methods": ["ft_transfer_call_no", "deposit"]})).collect();
    carol.call(&vault, "trust").args_json(json!({"add": add, "remove": []})).max_gas().transact().await?.into_result()?;
    let v = worker.view_account(&vault).await?;
    println!("after trusting 30 contracts: {} bytes, {:.4} NEAR", v.storage_usage, v.balance.as_yoctonear() as f64 / 1e24);
    assert!(spent < 0.11, "about a tenth of a NEAR");
    Ok(())
}
