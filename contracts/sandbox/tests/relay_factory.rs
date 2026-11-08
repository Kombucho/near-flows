//! Making relay vaults: a vault the brain refuses a record is never left behind
//! (it deletes itself and its NEAR goes back), the factory never pays for vaults
//! out of its own pocket, and a creator can pin the relay code it expects.
//! Build the wasm first: make build

mod common;
mod relayw;

use near_workspaces::types::NearToken;
use near_workspaces::AccountId;
use relayw::*;
use serde_json::{json, Value};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

/// The brain out of storage stake can't store a new vault's record. The vault
/// it can't serve is deleted, its NEAR back to bob, and create_vault says false.
#[tokio::test]
async fn a_vault_the_brain_refuses_is_deleted_and_its_deposit_refunded() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let bob = w.worker.dev_create_account().await?;
    let sink = w.worker.dev_create_account().await?;
    drain(w.brain.as_account(), sink.id(), 0).await?;
    let start = bob.view_account().await?.balance.as_yoctonear();
    let r = bob.call(w.factory.id(), "create_vault").args_json(json!({"name": "bob", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    let made: bool = r.clone().json()?;
    let vault: AccountId = format!("bob.{}", w.factory.id()).parse()?;
    assert!(w.brain.view("get_vault").args_json(json!({"vault": vault})).await?.json::<Option<Value>>()?.is_none(), "no record");
    assert!(!made, "create_vault says the vault wasn't made: {:?}", r.failures());
    w.worker.fast_forward(3).await?;
    assert!(w.worker.view_account(&vault).await.is_err(), "no vault left behind with bob's NEAR in it");
    assert_eq!(w.worker.view(w.factory.id(), "get_vault").args_json(json!({"owner": bob.id()})).await?.json::<Option<AccountId>>()?, None);
    let lost = start - bob.view_account().await?.balance.as_yoctonear();
    println!("refused vault: bob lost {:.5} NEAR (gas)", lost as f64 / NEAR as f64);
    assert!(lost < NEAR / 100, "bob got his deposit back: lost {lost}");

    // The brain topped up: bob makes his vault, under the same name.
    sink.transfer_near(w.brain.id(), NearToken::from_near(1)).await?.into_result()?;
    let r = bob.call(w.factory.id(), "create_vault").args_json(json!({"name": "bob", "keeper": null}))
        .deposit(NearToken::from_near(1)).max_gas().transact().await?;
    assert!(r.clone().json::<bool>()?, "{:?}", r.failures());
    assert!(w.brain.view("get_vault").args_json(json!({"vault": vault})).await?.json::<Option<Value>>()?.is_some());
    Ok(())
}

/// A creator may pin the relay code it audited: any other published code is refused.
#[tokio::test]
async fn a_creator_can_pin_the_relay_code_hash() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let hash: String = w.worker.view(w.factory.id(), "get_code_hash").await?.json::<Option<String>>()?.unwrap();
    let bob = w.worker.dev_create_account().await?;
    let wrong = "11111111111111111111111111111111";
    let r = bob.call(w.factory.id(), "create_vault").args_json(json!({"name": "bob", "keeper": null, "expected_code_hash": wrong}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(fails_with(&r, "isn't the relay code you expected"), "{:?}", r.failures());
    let key = format!("ed25519:{}", bs58::encode([9u8; 32]).into_string());
    let r = bob.call(w.factory.id(), "create_stealth_vault").args_json(json!({"name": "s1", "owner_key": key, "keeper": null, "expected_code_hash": wrong}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(fails_with(&r, "isn't the relay code you expected"), "{:?}", r.failures());
    let r = bob.call(w.factory.id(), "create_vault").args_json(json!({"name": "bob", "keeper": null, "expected_code_hash": hash}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(r.clone().json::<bool>()?, "{:?}", r.failures());
    Ok(())
}
