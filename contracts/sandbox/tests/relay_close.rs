//! Vault auto-close: an empty vault deletes itself and its NEAR goes back to the
//! owner; the name is tombstoned at the factory. Refused while anything is
//! still in it, while a step runs, for a stranger without auto-close, and for a
//! vault that ever touched another chain unless its owner confirms.
//! Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract};
use relayw::*;
use serde_json::{json, Value};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const ONE: u128 = 1_000_000_000_000_000_000;

fn near(y: u128) -> f64 {
    y as f64 / NEAR as f64
}

fn factory_of(w: &RelayWorld) -> AccountId {
    w.vault.as_str().split_once('.').unwrap().1.parse().unwrap()
}

async fn exists(w: &RelayWorld, a: &AccountId) -> bool {
    w.worker.view_account(a).await.is_ok()
}

/// A recipe that spends the node's NEAR and gets nothing back into the vault
/// (the mock's LST is sent to an account it can't credit): a NEAR-only flow.
fn spend_near(dex: &Contract) -> Value {
    json!({"label": "Stake", "calls": [{"call": "fn", "contract": dex.id(), "method": "deposit_and_stake", "args": "{}", "deposit": {"kind": "input"}, "tgas": 30}],
           "out": null, "min_out_per_in": null, "price": null})
}

async fn dex(w: &RelayWorld) -> anyhow::Result<(Contract, Contract, Account)> {
    let minter = w.worker.dev_create_account().await?;
    let lst = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    lst.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000 * ONE).to_string(), "name": "LST", "symbol": "LST", "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": lst.id()})).transact().await?.into_result()?;
    Ok((dex, lst, minter))
}

#[tokio::test]
async fn a_near_only_vault_auto_closes_after_its_flow_and_its_owner_gets_the_near_back() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let factory = factory_of(&w);
    let (dex, _, _) = dex(&w).await?;
    let bob = w.worker.dev_create_account().await?;
    let start = bob.view_account().await?.balance.as_yoctonear();
    let r = bob.call(&factory, "create_vault").args_json(json!({"name": "bob", "keeper": w.keeper.id()}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(r.clone().json::<bool>()?, "{:?}", r.failures());
    let vault: AccountId = format!("bob.{factory}").parse()?;
    let created = start - bob.view_account().await?.balance.as_yoctonear();
    bob.transfer_near(&vault, NearToken::from_near(1)).await?.into_result()?;
    x(&bob, &vault, "trust", json!({"add": [{"contract": dex.id(), "methods": ["deposit_and_stake"]}], "remove": []})).await?.into_result()?;
    let f: u32 = x(&bob, &vault, "save_flow", json!({"name": "spend", "nodes": [{"input": "near", "action": spend_near(&dex), "when": {"when": "now"}}]})).await?.json()?;
    x(&bob, &vault, "fund", json!({"flow_id": f, "node": 0, "amount": (NEAR / 2).to_string()})).await?.into_result()?;
    x(&bob, &vault, "set_auto_close", json!({"on": true})).await?.into_result()?;
    let r = x(&w.keeper, &vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());

    // The job is done: the keeper closes it; the NEAR can only go to bob.
    let before = bob.view_account().await?.balance.as_yoctonear();
    let (_, paid): (String, String) = w.brain.view("get_storage").args_json(json!({"vault": vault})).await?.json()?;
    let r = x(&w.keeper, &vault, "close", json!({})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    println!("GAS close: {:.1} Tgas", tgas(&r));
    assert!(!exists(&w, &vault).await, "the vault deleted itself");
    assert!(w.brain.view("get_vault").args_json(json!({"vault": vault})).await?.json::<Option<Value>>()?.is_none(), "its record is gone");
    let back = bob.view_account().await?.balance.as_yoctonear() - before;
    let end = bob.view_account().await?.balance.as_yoctonear();
    let net = start - end - NEAR / 2; // what bob spent, but the half NEAR the flow staked
    println!("CLOSE create {:.4} NEAR (deposit + gas), record refund {:.4}, back on close {:.4}, net cost {:.4} NEAR incl. every tx's gas",
        near(created), near(paid.parse()?), near(back), near(net));
    assert!(net < NEAR / 50, "net cost {:.4} NEAR", near(net));

    // The name is tombstoned: never made again, not even by bob, who may make another.
    assert!(w.worker.view(&factory, "is_closed").args_json(json!({"vault": vault})).await?.json::<bool>()?);
    let r = bob.call(&factory, "create_vault").args_json(json!({"name": "bob", "keeper": null})).deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(fails_with(&r, "never reused"), "{:?}", r.failures());
    let r = bob.call(&factory, "create_vault").args_json(json!({"name": "bob2", "keeper": null})).deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(r.json::<bool>()?, "bob may open a new vault under another name");
    // Nobody but the brain tombstones a name.
    let r = bob.call(&factory, "on_vault_closed").args_json(json!({"vault": format!("bob2.{factory}"), "owner": null})).transact().await?;
    assert!(fails_with(&r, "only the brain"));
    Ok(())
}

#[tokio::test]
async fn close_is_refused_while_a_token_a_flow_or_a_step_still_holds_anything() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let (dex, lst, minter) = dex(&w).await?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": dex.id(), "methods": ["deposit_and_stake"]}, {"contract": lst.id()}], "remove": []})).await?.into_result()?;
    // A stranger can't close a vault whose owner never turned auto-close on.
    let stranger = w.worker.dev_create_account().await?;
    assert!(fails_with(&x(&stranger, &w.vault, "close", json!({})).await?, "auto-close is off"));
    x(&w.alice, &w.vault, "set_auto_close", json!({"on": true})).await?.into_result()?;
    assert!(fails_with(&x(&stranger, &w.vault, "close", json!({"to": stranger.id()})).await?, "only where its owner said"));

    // A flow holding NEAR.
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "s", "nodes": [{"input": "near", "action": spend_near(&dex), "when": {"when": "now"}}]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (NEAR / 10).to_string()})).await?.into_result()?;
    assert!(fails_with(&x(&stranger, &w.vault, "close", json!({})).await?, "still holds funds"));

    // A step running: the close lands while the step's rounds are in flight.
    let step = w.keeper.call(&w.vault, "x").args(json!({"step": {"flow_id": f, "node": 0}}).to_string().into_bytes()).max_gas().transact_async().await?;
    let r = x(&stranger, &w.vault, "close", json!({})).await?;
    assert!(fails_with(&r, "a step is running"), "{:?}", r.failures());
    step.await?.into_result()?;

    // A token balance: 5 LST in the vault (lst is trusted, so the brain reads it).
    minter.call(lst.id(), "storage_deposit").args_json(json!({"account_id": w.vault})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    minter.call(lst.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (5 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    let r = x(&stranger, &w.vault, "close", json!({})).await?;
    assert!(fails_with(&r, &format!("holds {} of {}", 5 * ONE, lst.id())), "{:?}", r.failures());
    assert!(exists(&w, &w.vault).await);
    // Withdrawn, and the step's NEAR spent: now anyone may close it, to its owner.
    minter.call(lst.id(), "storage_deposit").args_json(json!({"account_id": w.alice.id()})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "withdraw", json!({"token": lst.id(), "amount": (5 * ONE).to_string()})).await?.into_result()?;
    let before = w.alice.view_account().await?.balance.as_yoctonear();
    let r = x(&stranger, &w.vault, "close", json!({})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(!exists(&w, &w.vault).await);
    println!("closed: {:.4} NEAR back to the owner", near(w.alice.view_account().await?.balance.as_yoctonear() - before));
    assert!(w.alice.view_account().await?.balance.as_yoctonear() > before + NEAR, "the vault's ~1.9 NEAR went to alice");
    Ok(())
}

#[tokio::test]
async fn a_vault_that_touched_another_chain_closes_only_with_its_owners_confirmation() -> anyhow::Result<()> {
    let w = relay_world().await?;
    x(&w.alice, &w.vault, "set_auto_close", json!({"on": true})).await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64, "max_fee": "1000000000", "max_tip": "100000000", "gas_cap": 1_000_000u64}))
        .await?.into_result()?;
    let rec: Value = w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json()?;
    assert_eq!(rec["crosschain"], json!(true), "marked for good");
    assert!(fails_with(&x(&w.keeper, &w.vault, "close", json!({})).await?, "used other chains"), "auto-close no longer applies");
    assert!(fails_with(&x(&w.alice, &w.vault, "set_auto_close", json!({"on": true})).await?, "used other chains"));
    assert!(fails_with(&x(&w.alice, &w.vault, "close", json!({})).await?, "confirm_remote"));
    let r = x(&w.alice, &w.vault, "close", json!({"confirm_remote": true})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(!exists(&w, &w.vault).await);
    Ok(())
}

#[tokio::test]
async fn a_stealth_vault_closes_by_its_owners_signed_command_to_the_account_it_names() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let factory = factory_of(&w);
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = format!("ed25519:{}", bs58::encode(key.verifying_key().as_bytes()).into_string());
    w.keeper.call(&factory, "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": public, "keeper": w.keeper.id()}))
        .deposit(NearToken::from_millinear(500)).max_gas().transact().await?.into_result()?;
    let sv: AccountId = format!("k7xq2m.{factory}").parse()?;
    let fresh = w.worker.dev_create_account().await?;
    let mut nonce = 0u64;
    let mut sign = |method: &str, args: Value| {
        nonce += 1;
        let args = args.to_string();
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 + 3_600_000_000_000;
        let text = format!("near-flows stealth v1\nvault: {sv}\nnonce: {nonce}\nexpires_ns: {expires}\ntgas: 60\nmethod: {method}\nargs: {args}");
        json!({"method": method, "args": args, "nonce": nonce.to_string(), "expires_ns": expires.to_string(), "tgas": 60, "signature": hex::encode(key.sign(text.as_bytes()).to_bytes())})
    };
    let exec = |s: Value| {
        let (k, sv) = (w.keeper.clone(), sv.clone());
        async move { anyhow::Ok(k.call(&sv, "exec").args_json(s).gas(near_workspaces::types::Gas::from_tgas(300)).transact().await?) }
    };
    // The relayer alone can't close it; a signed close must name where the NEAR goes.
    assert!(fails_with(&x(&w.keeper, &sv, "close", json!({"to": w.keeper.id()})).await?, "auto-close is off"));
    assert!(fails_with(&exec(sign("close", json!({}))).await?, "to"));
    let before = fresh.view_account().await?.balance.as_yoctonear();
    let r = exec(sign("close", json!({"to": fresh.id()}))).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(!exists(&w, &sv).await);
    let got = fresh.view_account().await?.balance.as_yoctonear() - before;
    println!("stealth vault closed: {:.4} NEAR to the account the owner signed for", near(got));
    assert!(got > NEAR / 4);
    Ok(())
}

/// A closed name must be tombstoned before the vault is gone: otherwise anyone
/// could make a new vault under it and control its derived addresses on other
/// chains. With the factory out of storage stake the tombstone can't be written,
/// so the close is refused and nothing is deleted; once it can, the close goes
/// through and the name is never made again. Making a vault never eats the
/// factory's own NEAR: each leaves enough for its list entry and its tombstone.
#[tokio::test]
async fn a_vault_is_deleted_only_after_its_name_is_tombstoned() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let factory = factory_of(&w);
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = format!("ed25519:{}", bs58::encode(key.verifying_key().as_bytes()).into_string());
    let before = spare(&w, &factory).await?;
    w.keeper.call(&factory, "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": public, "keeper": w.keeper.id()}))
        .deposit(NearToken::from_millinear(500)).max_gas().transact().await?.into_result()?;
    let sv: AccountId = format!("k7xq2m.{factory}").parse()?;
    assert!(exists(&w, &sv).await);
    let made = spare(&w, &factory).await?;
    println!("factory spare: {before} before, {made} after making a vault");

    let mut nonce = 0u64;
    let mut sign = |method: &str, args: Value| {
        nonce += 1;
        let args = args.to_string();
        let expires = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64 + 3_600_000_000_000;
        let text = format!("near-flows stealth v1\nvault: {sv}\nnonce: {nonce}\nexpires_ns: {expires}\ntgas: 60\nmethod: {method}\nargs: {args}");
        json!({"method": method, "args": args, "nonce": nonce.to_string(), "expires_ns": expires.to_string(), "tgas": 60, "signature": hex::encode(key.sign(text.as_bytes()).to_bytes())})
    };
    let exec = |s: Value| {
        let (k, sv) = (w.keeper.clone(), sv.clone());
        async move { anyhow::Ok(k.call(&sv, "exec").args_json(s).gas(near_workspaces::types::Gas::from_tgas(300)).transact().await?) }
    };
    let owner = w.worker.dev_create_account().await?;
    let sink = w.worker.dev_create_account().await?;
    drain(&w.factory, sink.id(), 0).await?;
    let r = exec(sign("close", json!({"to": owner.id()}))).await?;
    let closed = w.worker.view(&factory, "is_closed").args_json(json!({"vault": sv})).await?.json::<bool>()?;
    println!("factory out of stake: vault exists {}, name tombstoned {closed}", exists(&w, &sv).await);
    assert!(exists(&w, &sv).await || closed, "the vault is gone but its name isn't tombstoned");
    assert!(fails_with(&r, "couldn't tombstone"), "{:?}", r.failures());
    // Mallory can't take the name over.
    let mallory = w.worker.dev_create_account().await?;
    let mkey = format!("ed25519:{}", bs58::encode([8u8; 32]).into_string());
    sink.transfer_near(&factory, NearToken::from_millinear(10)).await?.into_result()?;
    let r = mallory.call(&factory, "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": mkey, "keeper": null}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(!r.json::<bool>().unwrap_or(false), "Mallory took over a closed vault's name");

    // The factory has room again: the owner closes, and the name is tombstoned for good.
    let r = exec(sign("close", json!({"to": owner.id()}))).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(!exists(&w, &sv).await);
    assert!(w.worker.view(&factory, "is_closed").args_json(json!({"vault": sv})).await?.json::<bool>()?);
    let r = mallory.call(&factory, "create_stealth_vault").args_json(json!({"name": "k7xq2m", "owner_key": mkey, "keeper": null}))
        .deposit(NearToken::from_millinear(100)).max_gas().transact().await?;
    assert!(fails_with(&r, "never reused"), "{:?}", r.failures());
    assert!(made >= before + 2_000_000_000_000_000_000_000, "a vault leaves the factory room for its list entry and tombstone");
    Ok(())
}
