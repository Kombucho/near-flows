//! The brain pays for every vault's record and charges it back. A record that
//! grows is paid for in the same round, or not stored at all: a vault can't
//! leave growth behind on the brain's own NEAR (which every vault shares).
//! Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract};
use relayw::*;
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR: u128 = 1_000_000_000_000_000_000_000_000;

async fn ft(w: &RelayWorld, minter: &Account, sym: &str) -> anyhow::Result<Contract> {
    let t = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn storage(w: &RelayWorld) -> anyhow::Result<(u128, u128)> {
    let (cost, paid): (String, String) = w.brain.view("get_storage").args_json(json!({"vault": w.vault})).await?.json()?;
    Ok((cost.parse()?, paid.parse()?))
}

/// A hidden step reveals its node (8 KB of args here) into the record while its
/// balances are read. The vault can't pay for that: the step is refused and the
/// record is left as it was, never grown on the brain's NEAR.
#[tokio::test]
async fn a_read_never_leaves_record_growth_the_vault_didnt_pay_for() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let minter = w.worker.dev_create_account().await?;
    let (wnear, usdc) = (ft(&w, &minter, "wNEAR").await?, ft(&w, &minter, "USDC").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": usdc.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [&w.vault, dex.id()] {
            minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    minter.call(usdc.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    minter.call(dex.id(), "set_rate").args_json(json!({"token_in": wnear.id(), "token_out": usdc.id(), "rate": (2 * NEAR).to_string()})).transact().await?.into_result()?;
    minter.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (100 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id()}], "remove": []})).await?.into_result()?;
    let pad = "p".repeat(8_000);
    let node = json!({"input": wnear.id(), "when": {"when": "now"}, "action": {"label": "Swap", "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}","pad":"{pad}"}}"#, usdc.id())}], "out": usdc.id(), "min_out_per_in": null, "price": null}});
    let salt = "aa".repeat(32);
    let print: String = w.brain.view("fingerprint_of").args_json(json!({"node": node, "salt": salt})).await?.json()?;
    let f: u32 = x(&w.alice, &w.vault, "save_hidden_flow", json!({"name": "secret", "inputs": [wnear.id()], "fingerprints": [print]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;

    // Alice takes her NEAR out, all but ~0.01 free: far less than 8 KB of record (0.08 NEAR).
    let v = w.worker.view_account(&w.vault).await?;
    let take = v.balance.as_yoctonear() - v.storage_usage as u128 * 10_000_000_000_000_000_000 - NEAR / 20 - NEAR / 100;
    x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": take.to_string()})).await?.into_result()?;
    let (cost, paid) = storage(&w).await?;
    assert!(cost <= paid, "paid up before the step: {cost} > {paid}");

    let r = x(&w.keeper, &w.vault, "step_hidden", json!({"flow_id": f, "node": 0, "reveal": node, "salt": salt})).await?;
    assert!(fails_with(&r, "can't pay for its storage"), "{:?}", r.failures());
    let rec: Value = w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json()?;
    let (cost, paid) = storage(&w).await?;
    println!("after the refused step: record costs {cost}, paid {paid}, a step running: {}", !rec["busy"].is_null());
    assert!(rec["busy"].is_null(), "no step left running with its revealed node stored");
    assert!(cost <= paid, "the record grew unpaid: costs {cost}, paid {paid}");
    // Anyone may have a vault pay what its record owes (nothing here: a no-op).
    let stranger = w.worker.dev_create_account().await?;
    let r = x(&stranger, &w.vault, "pay_storage", json!({})).await?;
    assert!(r.is_success(), "{:?}", r.failures());

    // With the NEAR back, the same step pays for its record and runs.
    w.alice.transfer_near(&w.vault, NearToken::from_near(1)).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step_hidden", json!({"flow_id": f, "node": 0, "reveal": node, "salt": salt})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    let (cost, paid) = storage(&w).await?;
    assert!(cost <= paid, "costs {cost}, paid {paid}");
    Ok(())
}
