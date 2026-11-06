//! A relay vault world shared by the cross-chain relay tests: brain, relay
//! factory, the relay published, Alice's vault with a keeper, the test signer
//! (mock_signer: real secp256k1 / Ed25519 keys standing in for the MPC network).
#![allow(dead_code)]

use crate::common::wasm;
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use serde_json::{json, Value};

pub struct RelayWorld {
    pub worker: Worker<Sandbox>,
    pub brain: Contract,
    pub alice: Account,
    pub keeper: Account,
    pub signer: Contract,
    pub vault: AccountId,
}

pub async fn relay_world() -> anyhow::Result<RelayWorld> {
    relay_world_fees(|_| Value::Null).await
}

/// The same world, with the brain's `fees` built from the treasury account (fees.rs).
pub async fn relay_world_fees(fees: impl Fn(&AccountId) -> Value) -> anyhow::Result<RelayWorld> {
    let worker = near_workspaces::sandbox().await?;
    let root = worker.root_account()?;
    let factory = root.create_subaccount("flows").initial_balance(NearToken::from_near(30)).transact().await?.into_result()?;
    let brain_acc = root.create_subaccount("brain").initial_balance(NearToken::from_near(15)).transact().await?.into_result()?;
    let brain = brain_acc.deploy(&wasm("brain")).await?.into_result()?;
    let treasury: AccountId = format!("treasury.{}", root.id()).parse()?;
    brain.call("new").args_json(json!({"factory": factory.id(), "fees": fees(&treasury)})).transact().await?.into_result()?;
    factory.deploy(&wasm("factory_relay")).await?.into_result()?;
    factory.call(factory.id(), "new").args_json(json!({"owner": factory.id(), "brain": brain.id()})).transact().await?.into_result()?;
    factory.call(factory.id(), "publish").args(wasm("relay")).deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let made: bool = alice.call(factory.id(), "create_vault").args_json(json!({"name": "alice", "keeper": keeper.id()}))
        .deposit(NearToken::from_near(2)).max_gas().transact().await?.into_result()?.json()?;
    assert!(made, "the vault was made");
    let vault: AccountId = format!("alice.{}", factory.id()).parse()?;
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    Ok(RelayWorld { worker, brain, alice, keeper, signer, vault })
}

/// A request to the relay vault: `{"<method>": <args>}`, the vault's own method names.
pub async fn x(who: &Account, vault: &AccountId, method: &str, args: Value) -> anyhow::Result<ExecutionFinalResult> {
    Ok(who.call(vault, "x").args(json!({ method: args }).to_string().into_bytes()).max_gas().transact().await?)
}

pub fn fails_with(r: &ExecutionFinalResult, why: &str) -> bool {
    r.is_failure() && format!("{:?}", r.failures()).contains(why)
}

pub fn tgas(r: &ExecutionFinalResult) -> f64 {
    r.total_gas_burnt.as_gas() as f64 / 1e12
}

/// A brain view about one vault. WHY: a relay vault keeps no state of its own,
/// so the full vault's `vault.view(method, args)` becomes
/// `brain.view(method, {"vault": .., ..args})`.
pub async fn view(brain: &Contract, vault: &AccountId, method: &str, args: Value) -> anyhow::Result<Value> {
    let mut args = if args.is_null() { json!({}) } else { args };
    args["vault"] = json!(vault);
    Ok(brain.view(method).args_json(args).await?.json()?)
}
