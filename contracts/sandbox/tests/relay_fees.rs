//! Fees through relay + brain (brain/src/fees.rs): the protocol's share of what
//! a step used, the flat fee per cross-chain signature, and the keeper's price
//! for runs it did. Every amount is checked against its own computation here.
//! Build the wasm first: make build

mod common;
mod relayw;

use common::wasm;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract};
use relayw::*;
use serde_json::{json, Value};
use solguard::testkit::*;
use solguard::*;

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const CENTI: u128 = NEAR / 100;
const BPS: u128 = 10;

fn fees(treasury: &AccountId) -> Value {
    json!({"treasury": treasury, "admin": treasury, "cap_bps": 30, "bps": BPS, "per_signature": CENTI.to_string(), "per_signature_cap": (5 * CENTI).to_string()})
}

struct World {
    w: RelayWorld,
    treasury: Account,
    minter: Account,
    wnear: Contract,
    usdc: Contract,
    dex: Contract,
}

async fn ft(w: &RelayWorld, minter: &Account, sym: &str) -> anyhow::Result<Contract> {
    let t = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

async fn reg(minter: &Account, t: &Contract, who: &AccountId) -> anyhow::Result<()> {
    minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
    Ok(())
}

async fn bal(t: &Contract, who: &AccountId) -> anyhow::Result<u128> {
    Ok(t.view("ft_balance_of").args_json(json!({"account_id": who})).await?.json::<String>()?.parse()?)
}

async fn near_of(w: &RelayWorld, who: &AccountId) -> anyhow::Result<u128> {
    Ok(w.worker.view_account(who).await?.balance.as_yoctonear())
}

/// Alice's vault on a brain charging 10 bps and 0.01 NEAR per signature; the treasury
/// isn't registered with any token yet. 1,000 wNEAR, a dex paying 2 USDC per wNEAR.
async fn world() -> anyhow::Result<World> {
    let w = relay_world_fees(fees).await?;
    let treasury = w.worker.root_account()?.create_subaccount("treasury").initial_balance(NearToken::from_near(5)).transact().await?.into_result()?;
    let minter = w.worker.dev_create_account().await?;
    let (wnear, usdc) = (ft(&w, &minter, "wNEAR").await?, ft(&w, &minter, "USDC").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": usdc.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [dex.id(), &w.vault, w.alice.id()] {
            reg(&minter, t, who).await?;
        }
        minter.call(t.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    }
    minter.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    w.alice.call(dex.id(), "set_rate").args_json(json!({"token_in": wnear.id(), "token_out": usdc.id(), "rate": (2 * NEAR).to_string()})).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id(), "methods": ["deposit_and_stake"]}], "remove": []})).await?.into_result()?;
    Ok(World { w, treasury, minter, wnear, usdc, dex })
}

fn swap(m: &World, when: &str) -> Value {
    json!({"input": m.wnear.id(), "when": {"when": when}, "action": {"label": "Swap", "calls": [{"call": "ft", "receiver": m.dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, m.usdc.id())}], "out": m.usdc.id(), "min_out_per_in": null, "price": null}})
}

fn fee_events(r: &ExecutionFinalResult, kind: &str) -> Vec<Value> {
    r.logs().iter().filter(|l| l.contains(r#""event":"fee""#) && l.contains(&format!(r#""kind":"{kind}""#)))
        .map(|l| serde_json::from_str::<Value>(l.trim_start_matches("EVENT_JSON:")).unwrap()["data"][0].clone()).collect()
}

async fn owed(m: &World, token: &AccountId) -> anyhow::Result<u128> {
    let rec: Value = m.w.brain.view("get_vault").args_json(json!({"vault": m.w.vault})).await?.json()?;
    Ok(rec["owed_fees"].as_array().unwrap().iter().find(|p| p[0] == json!(token)).map_or(0, |p| p[1].as_str().unwrap().parse().unwrap()))
}

#[tokio::test]
async fn the_protocol_fee_is_bps_of_what_a_step_used_set_aside_then_settled_to_the_treasury() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "s", "nodes": [swap(&m, "now")]})).await?.json()?;
    // 123.456789 wNEAR: the fee rounds down, computed here independently: used * 10 / 10,000.
    let used = 123_456_789 * ONE / 1_000_000;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": used.to_string()})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    println!("GAS step with the protocol fee: {:.1} Tgas", tgas(&r));
    let fee = used * BPS / 10_000;
    let ev = fee_events(&r, "protocol");
    assert_eq!(ev.len(), 1, "{:?}", r.logs());
    assert_eq!(ev[0]["fee"], json!(fee.to_string()));
    println!("FEE protocol: {used} wNEAR units used -> {fee} fee ({} bps)", BPS);
    assert_eq!(owed(&m, m.wnear.id()).await?, fee, "set aside in the vault's record");
    // Out of the free balance at once: the owner can't withdraw it.
    let free = 1_000 * ONE - used - fee;
    let alloc: String = w.brain.view("get_allocated").args_json(json!({"vault": w.vault, "token": m.wnear.id()})).await?.json()?;
    assert_eq!(alloc, fee.to_string());
    assert!(fails_with(&x(&w.alice, &w.vault, "withdraw", json!({"token": m.wnear.id(), "amount": (free + 1).to_string()})).await?, "not enough free balance"));
    x(&w.alice, &w.vault, "withdraw", json!({"token": m.wnear.id(), "amount": free.to_string()})).await?.into_result()?;

    // The treasury isn't registered with wNEAR: settling fails and the fee stays owed.
    let stranger = w.worker.dev_create_account().await?;
    let r = x(&stranger, &w.vault, "settle_fees", json!({"token": m.wnear.id()})).await?;
    assert_eq!(fee_events(&r, "settle_failed").len(), 1, "{:?}", r.logs());
    assert_eq!(owed(&m, m.wnear.id()).await?, fee);
    reg(&m.minter, &m.wnear, m.treasury.id()).await?;
    let r = x(&stranger, &w.vault, "settle_fees", json!({"token": m.wnear.id()})).await?;
    assert_eq!(fee_events(&r, "settled").len(), 1, "{:?}", r.logs());
    assert_eq!(bal(&m.wnear, m.treasury.id()).await?, fee, "exactly the fee");
    assert_eq!(owed(&m, m.wnear.id()).await?, 0);
    // Conservation: 1,000 = what the dex took + what alice withdrew + the treasury's; nothing left over.
    assert_eq!(bal(&m.wnear, &w.vault).await?, 0);
    assert_eq!(bal(&m.wnear, m.dex.id()).await? - 1_000_000 * ONE + bal(&m.wnear, w.alice.id()).await? + bal(&m.wnear, m.treasury.id()).await?, 1_000 * ONE);
    assert_eq!(bal(&m.usdc, &w.vault).await?, 2 * used, "the output is untouched");

    // A failed step pays nothing: the dex now pays 0.
    let g: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "g", "nodes": [swap(&m, "now")]})).await?.json()?;
    m.minter.call(m.wnear.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (10 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": g, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    w.alice.call(m.dex.id(), "set_rate").args_json(json!({"token_in": m.wnear.id(), "token_out": m.usdc.id(), "rate": "0"})).transact().await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": g, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")) && fee_events(&r, "protocol").is_empty(), "{:?}", r.logs());
    assert_eq!(owed(&m, m.wnear.id()).await?, 0);

    // NEAR in: the fee goes to the treasury as NEAR in the step's own answer.
    let n: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "n", "nodes": [{"input": "near", "when": {"when": "now"}, "action": {"label": "Stake",
        "calls": [{"call": "fn", "contract": m.dex.id(), "method": "deposit_and_stake", "args": "{}", "deposit": {"kind": "input"}, "tgas": 30}],
        "out": m.usdc.id(), "min_out_per_in": null, "price": null}}]})).await?.json()?;
    let staked = 7 * NEAR / 10;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": n, "node": 0, "amount": staked.to_string()})).await?.into_result()?;
    let before = near_of(w, m.treasury.id()).await?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": n, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    let got = near_of(w, m.treasury.id()).await? - before;
    println!("FEE protocol: {staked} yoctoNEAR used -> {got} to the treasury");
    assert_eq!(got, staked * BPS / 10_000);
    Ok(())
}

#[tokio::test]
async fn fees_move_only_within_their_caps_and_only_by_the_fee_admin() -> anyhow::Result<()> {
    let m = world().await?;
    let b = &m.w.brain;
    let get = || async { anyhow::Ok(b.view("get_fees").await?.json::<Value>()?) };
    let f = get().await?;
    assert_eq!((f["bps"].clone(), f["cap_bps"].clone(), f["per_signature"].clone()), (json!(10), json!(30), json!(CENTI.to_string())));
    let set = |who: &Account, bps: u16, sig: u128| {
        let (who, b) = (who.clone(), b.id().clone());
        async move { anyhow::Ok(who.call(&b, "set_fee").args_json(json!({"bps": bps, "per_signature": sig.to_string()})).transact().await?) }
    };
    assert!(fails_with(&set(&m.w.alice, 5, CENTI).await?, "fee admin only"));
    assert!(fails_with(&set(&m.treasury, 31, CENTI).await?, "fee over its cap"));
    assert!(fails_with(&set(&m.treasury, 10, 5 * CENTI + 1).await?, "signature fee over its cap"));
    // A raise waits 24 hours, in plain sight; the fees in force don't move meanwhile.
    set(&m.treasury, 30, 5 * CENTI).await?.into_result()?;
    let f = get().await?;
    assert_eq!((f["bps"].clone(), f["per_signature"].clone()), (json!(10), json!(CENTI.to_string())), "not raised yet: {f}");
    assert_eq!((f["pending"]["bps"].clone(), f["pending"]["per_signature"].clone()), (json!(30), json!((5 * CENTI).to_string())), "{f}");
    assert!(f["pending"]["active_at_ns"].as_str().unwrap().parse::<u64>()? > 0);
    // A cut is at once, and drops the pending raise.
    set(&m.treasury, 0, 0).await?.into_result()?;
    let f = get().await?;
    assert_eq!(f["bps"], json!(0), "lowered");
    assert!(f["pending"].is_null(), "{f}");
    // No brain starts with a signature fee cap over 0.1 NEAR (the bps cap is 10%).
    let other = m.w.worker.dev_deploy(&wasm("brain")).await?;
    let fees = json!({"treasury": m.treasury.id(), "admin": m.treasury.id(), "cap_bps": 30, "bps": 10, "per_signature": "0", "per_signature_cap": (10 * CENTI + 1).to_string()});
    let r = other.call("new").args_json(json!({"factory": m.w.vault, "fees": fees})).transact().await?;
    assert!(fails_with(&r, "signature fee cap over 0.1 NEAR"), "{:?}", r.failures());
    Ok(())
}

#[tokio::test]
async fn a_cross_chain_signature_costs_the_flat_fee_and_is_refused_without_the_near() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me = pubkey(w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap());
    for t in ["wsol_wrap", "wsol_sync"] {
        x(&w.alice, &w.vault, "sol_template", json!({"template": common::sol_template(t, &[])})).await?.into_result()?;
    }
    let wsol = ata_of(&me, &pubkey("So11111111111111111111111111111111111111112"));
    let mut transfer = vec![2u8, 0, 0, 0];
    transfer.extend(500_000_000u64.to_le_bytes());
    let msg = Tx::new(me).ix(pubkey(SYSTEM_PROGRAM), vec![me, wsol], transfer).ix(pubkey("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA"), vec![wsol], vec![17]).bytes();
    let b64 = { use base64::Engine; base64::engine::general_purpose::STANDARD.encode(&msg) };
    let before = near_of(w, m.treasury.id()).await?;
    let r = x(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": b64})).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());
    println!("GAS sol_sign_tx with the signature fee: {:.1} Tgas", tgas(&r));
    assert_eq!(near_of(w, m.treasury.id()).await? - before, CENTI, "exactly 0.01 NEAR");
    // Withdraw the free NEAR down to under 0.001: the next signature is refused before anything is signed.
    let mut free = near_of(w, &w.vault).await? - NEAR / 20; // less the 0.05 reserve: an upper bound
    let mut r = x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": free.to_string()})).await?;
    while r.is_failure() && free > CENTI / 10 {
        free -= CENTI / 10;
        r = x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": free.to_string()})).await?;
    }
    assert!(r.is_success(), "withdrew {free}");
    let r = x(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": b64})).await?;
    assert!(fails_with(&r, "signature fee") && !r.logs().iter().any(|l| l.contains("sol_signed")), "{:?}", r.failures());
    Ok(())
}

#[tokio::test]
async fn the_keeper_is_paid_its_price_only_for_runs_it_did_that_worked() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    assert!(fails_with(&x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": (5 * CENTI + 1).to_string()})).await?, "at most 0.05 NEAR"));
    assert!(fails_with(&x(&w.keeper, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": (5 * CENTI).to_string()})).await?, "owner only"), "a keeper can't set its own price");
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": CENTI.to_string()})).await?.into_result()?;
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "s", "nodes": [swap(&m, "now")]})).await?.json()?;
    // What a run cost the VAULT in NEAR: the vault pays no gas (the signer does), so on a
    // token step the only NEAR that leaves it is the keeper's fee.
    let run = |who: &Account| {
        let (who, v, worker) = (who.clone(), w.vault.clone(), w.worker.clone());
        async move {
            let before = worker.view_account(&v).await?.balance.as_yoctonear();
            let r = x(&who, &v, "step", json!({"flow_id": f, "node": 0})).await?;
            let paid = before as i128 - worker.view_account(&v).await?.balance.as_yoctonear() as i128;
            anyhow::Ok((r, paid))
        }
    };
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let (r, paid) = run(&w.keeper).await?;
    assert_eq!(fee_events(&r, "keeper").len(), 1, "{:?}", r.logs());
    let with_fee = paid;
    // Owner-run: nothing for the keeper.
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(fee_events(&r, "keeper").is_empty());
    // A run that failed: nothing.
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    w.alice.call(m.dex.id(), "set_rate").args_json(json!({"token_in": m.wnear.id(), "token_out": m.usdc.id(), "rate": "0"})).transact().await?.into_result()?;
    let (r, _) = run(&w.keeper).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")) && fee_events(&r, "keeper").is_empty(), "{:?}", r.logs());
    // Your own keeper at 0: nothing paid, nothing logged.
    w.alice.call(m.dex.id(), "set_rate").args_json(json!({"token_in": m.wnear.id(), "token_out": m.usdc.id(), "rate": (2 * NEAR).to_string()})).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id()})).await?.into_result()?;
    let (r, without) = run(&w.keeper).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)) && fee_events(&r, "keeper").is_empty(), "{:?}", r.logs());
    println!("FEE keeper: the vault paid {with_fee} yoctoNEAR for the keeper's run (price {CENTI}), {without} at price 0");
    // (+1 yocto each: the swap's ft_transfer_call carries 1 yoctoNEAR.)
    assert_eq!((with_fee, without), (CENTI as i128 + 1, 1));
    Ok(())
}

/// Withdraw the vault's free NEAR down to under 0.001 NEAR (the 0.05 reserve and storage stay).
async fn drain_near(w: &RelayWorld) -> anyhow::Result<()> {
    let mut free = near_of(w, &w.vault).await? - NEAR / 20;
    let mut r = x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": free.to_string()})).await?;
    while r.is_failure() && free > CENTI / 10 {
        free -= CENTI / 10;
        r = x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": free.to_string()})).await?;
    }
    assert!(r.is_success(), "withdrew {free}");
    Ok(())
}

#[tokio::test]
async fn a_vault_that_cant_pay_its_keeper_gets_no_free_work() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": CENTI.to_string()})).await?.into_result()?;
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "s", "nodes": [swap(&m, "now")]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    drain_near(w).await?;

    // The keeper's step is refused before anything moves: same NEAR, same tokens, same record, not busy.
    let record = || async { anyhow::Ok(w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json::<Value>()?) };
    let (near0, wnear0, usdc0, rec0) = (near_of(w, &w.vault).await?, bal(&m.wnear, &w.vault).await?, bal(&m.usdc, &w.vault).await?, record().await?);
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(fails_with(&r, "keeper fee unpaid") && fails_with(&r, "run your own keeper for free: https://github.com/Kombucho/near-flows (AGENTS.md)"), "{:?}", r.failures());
    assert!(!r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?}", r.logs());
    assert_eq!((near_of(w, &w.vault).await?, bal(&m.wnear, &w.vault).await?, bal(&m.usdc, &w.vault).await?), (near0, wnear0, usdc0), "funds untouched");
    let rec = record().await?;
    assert_eq!(rec, rec0, "ledger untouched");
    assert!(rec["busy"].is_null(), "not left busy");

    // The owner can still run it, keeper fee or not.
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)) && fee_events(&r, "keeper").is_empty(), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(bal(&m.usdc, &w.vault).await?, usdc0 + 20 * ONE);

    // Your own keeper at fee 0 runs the drained vault as before.
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id()})).await?.into_result()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)) && fee_events(&r, "keeper").is_empty(), "{:?} {:?}", r.logs(), r.failures());

    // Topped up: the keeper's run pays exactly its price (+1 yocto: the swap's ft_transfer_call).
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": CENTI.to_string()})).await?.into_result()?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(1)).await?.into_result()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let before = near_of(w, &w.vault).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let ev = fee_events(&r, "keeper");
    assert_eq!(ev.len(), 1, "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(ev[0]["fee"], json!(CENTI.to_string()));
    assert_eq!(before - near_of(w, &w.vault).await?, CENTI + 1, "the vault paid exactly the price");
    println!("FEE keeper unpaid: refused up front, record and funds untouched; topped up, paid exactly {CENTI}");
    Ok(())
}

fn stake_node(m: &World, deposit: Value) -> Value {
    json!({"input": "near", "when": {"when": "now"}, "action": {"label": "Stake",
        "calls": [{"call": "fn", "contract": m.dex.id(), "method": "deposit_and_stake", "args": "{}", "deposit": deposit, "tgas": 30}],
        "out": m.usdc.id(), "min_out_per_in": null, "price": null}})
}

/// A flow holding `amount` NEAR: a stake node, funded and never run.
async fn near_flow(m: &World, amount: u128) -> anyhow::Result<u32> {
    let w = &m.w;
    let k: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "held", "nodes": [stake_node(m, json!({"kind": "input"}))]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": k, "node": 0, "amount": amount.to_string()})).await?.into_result()?;
    Ok(k)
}

/// Put all but under 0.0001 NEAR of the vault's free NEAR into node 0 of `flow`. Returns how much.
async fn fund_all_near(w: &RelayWorld, flow: u32, upper: u128) -> anyhow::Result<u128> {
    let fund = |a: u128| x(&w.alice, &w.vault, "fund", json!({"flow_id": flow, "node": 0, "amount": a.to_string()}));
    let mut a = upper;
    while fund(a).await?.is_failure() {
        a -= CENTI / 10;
    }
    let mut total = a;
    while fund(CENTI / 100).await?.is_success() {
        total += CENTI / 100;
    }
    Ok(total)
}

/// Security report: settle_fees and intents_withdraw had no idle() check, so either
/// landing between a step's two balance reads corrupted what the step measured.
#[tokio::test]
async fn nobody_moves_the_vaults_tokens_while_a_step_is_measuring_them() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    reg(&m.minter, &m.wnear, m.treasury.id()).await?;
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "s", "nodes": [swap(&m, "now")]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?.into_result()?;
    assert!(owed(&m, m.wnear.id()).await? > 0, "a wNEAR fee to settle");
    // Intents set up (the verifier here is a plain account: only the brain's refusal matters).
    let verifier = w.worker.dev_create_account().await?;
    let _ = x(&w.alice, &w.vault, "intents_setup", json!({"verifier": verifier.id(), "signer": w.signer.id()})).await?;
    assert!(!w.brain.view("get_intents").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["key"].is_null());

    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let step = w.keeper.call(&w.vault, "x").args(json!({"step": {"flow_id": f, "node": 0}}).to_string().into_bytes()).max_gas().transact_async().await?;
    let stranger = w.worker.dev_create_account().await?;
    let (settle, back) = tokio::join!(
        x(&stranger, &w.vault, "settle_fees", json!({"token": m.wnear.id()})),
        x(&w.keeper, &w.vault, "intents_withdraw", json!({"token": m.usdc.id(), "amount": "1"}))
    );
    let (settle, back) = (settle?, back?);
    let r = step.await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    assert!(fails_with(&settle, "a step is running"), "a stranger's settle mid-step: {:?} {:?}", settle.logs(), settle.failures());
    assert!(fails_with(&back, "a step is running"), "a keeper's intents_withdraw mid-step: {:?}", back.failures());
    // Idle again: the settle goes through.
    let r = x(&stranger, &w.vault, "settle_fees", json!({"token": m.wnear.id()})).await?;
    assert_eq!(fee_events(&r, "settled").len(), 1, "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

/// Security report: settle_fees(near) paid `owed.min(free_near + owed)`, i.e. all of
/// it, out of the reserve and other flows' NEAR when the vault had no free NEAR.
#[tokio::test]
async fn settling_owed_near_fees_never_takes_the_reserve_or_a_flows_near() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    let held = NEAR / 2;
    near_flow(&m, held).await?;
    let n: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "n", "nodes": [stake_node(&m, json!({"kind": "input"}))]})).await?.json()?;
    let staked = fund_all_near(w, n, near_of(w, &w.vault).await? - NEAR / 20 - held).await?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": n, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    let fee = owed(&m, &"near".parse()?).await?;
    assert_eq!(fee, staked * BPS / 10_000, "no free NEAR to pay it from: owed");

    let stranger = w.worker.dev_create_account().await?;
    let before = near_of(w, &w.vault).await?;
    let r = x(&stranger, &w.vault, "settle_fees", json!({"token": "near"})).await?;
    let after = near_of(w, &w.vault).await?;
    assert!(fails_with(&r, "nothing to settle with"), "{:?} {:?}", r.logs(), r.failures());
    assert!(after + CENTI / 100 >= before, "the settle took {} yoctoNEAR the vault didn't have free", before - after);
    assert_eq!(owed(&m, &"near".parse()?).await?, fee);

    // Topped up: it settles exactly what's owed.
    w.alice.transfer_near(&w.vault, NearToken::from_near(1)).await?.into_result()?;
    let t0 = near_of(w, m.treasury.id()).await?;
    let r = x(&stranger, &w.vault, "settle_fees", json!({"token": "near"})).await?;
    assert_eq!(fee_events(&r, "settled").len(), 1, "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(near_of(w, m.treasury.id()).await? - t0, fee);
    assert_eq!(owed(&m, &"near".parse()?).await?, 0);
    Ok(())
}

/// Security report: a recipe's Deposit::Fixed was attached with no free-NEAR check,
/// paid out of other flows' NEAR (and the reserve) every run.
#[tokio::test]
async fn a_recipes_fixed_deposit_comes_from_free_near_never_from_other_flows() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    near_flow(&m, NEAR).await?;
    drain_near(w).await?;
    let node = json!({"input": m.wnear.id(), "when": {"when": "now"}, "action": {"label": "Stake then swap", "calls": [
        {"call": "fn", "contract": m.dex.id(), "method": "deposit_and_stake", "args": "{}", "deposit": {"kind": "fixed", "yocto": (NEAR / 2).to_string()}, "tgas": 30},
        {"call": "ft", "receiver": m.dex.id(), "tgas": 50, "msg": format!(r#"{{"token_out":"{}","min_out":"1"}}"#, m.usdc.id())}],
        "out": m.usdc.id(), "min_out_per_in": null, "price": null}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "f", "nodes": [node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let before = near_of(w, &w.vault).await?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let after = near_of(w, &w.vault).await?;
    assert!(after + CENTI / 100 >= before, "the step attached {} yoctoNEAR of another flow's NEAR", before - after);
    assert!(fails_with(&r, "free NEAR"), "{:?} {:?}", r.logs(), r.failures());
    // With the NEAR free, it runs and pays it.
    w.alice.transfer_near(&w.vault, NearToken::from_near(1)).await?.into_result()?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

/// Security report: a collector that collected nothing counted as a success, and the
/// keeper was paid for it (up to 0.05 NEAR a minute, 72 NEAR a day).
#[tokio::test]
async fn the_keeper_isnt_paid_for_a_collector_run_that_collected_nothing() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    x(&w.alice, &w.vault, "set_keeper", json!({"keeper": w.keeper.id(), "keeper_fee": CENTI.to_string()})).await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": m.dex.id(), "methods": ["deposit_and_stake", "set_rate"]}], "remove": []})).await?.into_result()?;
    let every = json!({"when": "every", "every_ns": 60_000_000_000u64, "chunk": "1"});
    let empty = json!({"input": "none", "when": every, "action": {"label": "Claim nothing", "calls": [{"call": "fn", "contract": m.dex.id(), "method": "set_rate",
        "args": json!({"token_in": m.wnear.id(), "token_out": m.usdc.id(), "rate": (2 * NEAR).to_string()}).to_string(), "deposit": {"kind": "none"}, "tgas": 10}],
        "out": m.usdc.id(), "min_out_per_in": null, "price": null}});
    let full = json!({"input": "none", "when": every, "action": {"label": "Claim", "calls": [{"call": "fn", "contract": m.dex.id(), "method": "deposit_and_stake",
        "args": "{}", "deposit": {"kind": "fixed", "yocto": CENTI.to_string()}, "tgas": 30}], "out": m.usdc.id(), "min_out_per_in": null, "price": null}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "c", "nodes": [empty, full]})).await?.json()?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert!(fee_events(&r, "keeper").is_empty(), "paid for collecting nothing: {:?}", r.logs());
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 1})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(fee_events(&r, "keeper").len(), 1, "paid for collecting {CENTI} USDC units: {:?}", r.logs());
    Ok(())
}

/// Security report: free NEAR is the balance the vault reported when the request
/// came in; two requests in parallel both saw it whole and both spent it.
#[tokio::test]
async fn parallel_requests_cant_spend_the_same_free_near_twice() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    let held = NEAR;
    near_flow(&m, held).await?;
    let acc = w.worker.view_account(&w.vault).await?;
    let storage = acc.storage_usage as u128 * 10u128.pow(19);
    let free = acc.balance.as_yoctonear() - storage - NEAR / 20 - held;
    let amount = free - CENTI / 10;
    let send = || x(&w.alice, &w.vault, "withdraw", json!({"token": "near", "amount": amount.to_string()}));
    let (r1, r2) = tokio::join!(send(), send());
    let (r1, r2) = (r1?, r2?);
    println!("parallel withdrawals of {amount}: {} {}", r1.is_success(), r2.is_success());
    assert!(r1.is_success() != r2.is_success(), "exactly one of two parallel withdrawals of the free NEAR: {:?} {:?}", r1.failures(), r2.failures());
    let left = near_of(w, &w.vault).await?;
    assert!(left >= held + NEAR / 20 + storage, "the flow's NEAR and the reserve stayed: {left}");
    Ok(())
}

/// Security report: a step's input used only in part (a partial fill) lost the rest
/// to the free balance instead of its node.
#[tokio::test]
async fn a_partial_fill_returns_the_unused_input_to_its_node() -> anyhow::Result<()> {
    let m = world().await?;
    let w = &m.w;
    let node = json!({"input": m.wnear.id(), "when": {"when": "now"}, "action": {"label": "Swap 60%", "calls": [{"call": "ft", "receiver": m.dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"1","use_bps":6000}}"#, m.usdc.id())}], "out": m.usdc.id(), "min_out_per_in": null, "price": null}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "p", "nodes": [node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let r = x(&w.alice, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#) && l.contains(&format!(r#""used":"{}""#, 6 * ONE))), "{:?} {:?}", r.logs(), r.failures());
    let flow: Value = w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json()?;
    assert_eq!(flow["ledger"]["nodes"][0]["inbox"], json!((4 * ONE).to_string()), "the unused 4 wNEAR are the node's again");
    let alloc: String = w.brain.view("get_allocated").args_json(json!({"vault": w.vault, "token": m.wnear.id()})).await?.json()?;
    assert_eq!(alloc.parse::<u128>()?, 4 * ONE + 6 * ONE * BPS / 10_000, "the node's 4 and the fee set aside");
    Ok(())
}
