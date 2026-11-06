//! A relay vault against REAL protocol code: Rhea DEX, Rhea lending, LiNEAR and
//! wNEAR, as deployed on testnet (fetched by scripts/fetch-protocols.sh), running
//! in a local NEAR node. Every node is the node library's (nodes/library.json,
//! same calls and gas) and every request carries exactly 300 Tgas, mainnet's cap.
//! Build our wasm first: make build

mod common;
mod protocolsw;
mod rhea;
mod relayw;

use near_workspaces::types::NearToken;
use protocolsw::*;
use serde_json::{json, Value};

#[tokio::test]
async fn near_is_wrapped_then_half_is_swapped_on_rhea() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(50)).await?.into_result()?;
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 5000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, Some(19 * SCALE / 10 / 1_000_000)), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    let r = step(&w, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(balance(&w.wrap, &w.vault).await?, 10 * NEAR, "10 NEAR wrapped");
    let r = step(&w, f, 1).await?;
    assert!(stepped(&r), "{:?} {:?}", r.logs(), r.failures());
    let usdc = balance(&w.usdc, &w.vault).await?;
    // 5 wNEAR into a 1,000 : 2,000 pool with a 0.25% fee: a bit under 10 USDC.
    println!("5 wNEAR -> {:.4} USDC on Rhea", usdc as f64 / ONE as f64);
    assert!(usdc > 9_9 * ONE / 10 && usdc < 10 * ONE, "{usdc}");
    assert_eq!(balance(&w.wrap, &w.vault).await?, 5 * NEAR, "the unrouted half stays free");
    Ok(())
}

#[tokio::test]
async fn rhea_refuses_a_swap_below_the_minimum_and_the_money_stays_in_the_flow() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(20)).await?.into_result()?;
    // Demand 2.5 USDC per wNEAR from a pool paying ~2.
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, Some(25 * SCALE / 10 / 1_000_000)), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 5 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")), "{:?}", r.logs());
    assert_eq!(balance(&w.wrap, &w.vault).await?, 5 * NEAR, "Rhea refunded, nothing lost");
    let s: Value = w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json()?;
    assert_eq!(s["ledger"]["nodes"][1]["inbox"], (5 * NEAR).to_string(), "still waiting in the swap node");
    Ok(())
}

#[tokio::test]
async fn a_price_trigger_reads_rhea_and_waits() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(20)).await?.into_result()?;
    // Sell wNEAR only when Rhea pays 3+ USDC per wNEAR (it pays ~2).
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": rhea_swap(&w, &w.wrap, &w.usdc, None), "when": {"when": "price_above", "price": (3 * SCALE / 1_000_000).to_string()}},
    ])).await?;
    fund(&w, f, 0, 5 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(format!("{:?}", r.failures()).contains("NotYet"), "{:?}", r.failures());
    assert_eq!(balance(&w.wrap, &w.vault).await?, 5 * NEAR);
    Ok(())
}

#[tokio::test]
async fn native_near_is_staked_with_real_linear() -> anyhow::Result<()> {
    let w = setup().await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(20)).await?.into_result()?;
    let f = save(&w, json!([{"input": "near", "action": linear_stake(&w), "when": {"when": "now"}}])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    let r = step(&w, f, 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let got = balance(&w.linear, &w.vault).await?;
    let price: String = w.linear.view("ft_price").await?.json()?;
    println!("10 NEAR -> {:.4} LiNEAR (price {price})", got as f64 / NEAR as f64);
    assert!(got > 9 * NEAR && got <= 10 * NEAR, "{got}");
    let _ = (&w.usdc, &w.rhea, &w.worker, &w.lp);
    Ok(())
}

#[tokio::test]
async fn borrow_usdc_against_wnear_on_real_rhea_lending() -> anyhow::Result<()> {
    let w = setup().await?;
    let lend = lending(&w).await?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(30)).await?.into_result()?;
    // 10 NEAR -> wNEAR -> collateral; borrow 3 USDC per wNEAR (worth $9.60, so ~31% LTV).
    let f = save(&w, json!([
        {"input": "near", "action": wrap_near(&w), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10000}]},
        {"input": w.wrap.id(), "action": supply_and_borrow(&w, &lend, 3 * ONE), "when": {"when": "now"}},
    ])).await?;
    fund(&w, f, 0, 10 * NEAR).await?;
    step(&w, f, 0).await?.into_result()?;
    let r = step(&w, f, 1).await?;
    assert!(stepped(&r), "{:?} {:?}", r.logs(), r.failures());
    assert_eq!(balance(&w.usdc, &w.vault).await?, 30 * ONE, "borrowed 30 USDC into the vault");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": &w.vault})).await?.json()?;
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
    let usdc = balance(&w.usdc, &w.vault).await?;
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
    assert_eq!(balance(&w.usdc, &w.vault).await?, usdc, "no new loan");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": &w.vault})).await?.json()?;
    assert_eq!(acct["collateral"][0]["balance"], (20 * NEAR).to_string(), "all 20 wNEAR is collateral");

    // A repay the protocol refuses (bad message) must stop the recipe: no collateral released.
    let mut bad = repay_and_release(&w, &lend, NEAR / 3 * (SCALE / ONE));
    bad["calls"][0]["msg"] = json!("\"OnlyRepay\"");
    let wrap_before = balance(&w.wrap, &w.vault).await?;
    let b = save(&w, json!([{"input": w.usdc.id(), "action": bad, "when": {"when": "manual"}}])).await?;
    fund(&w, b, 0, 30 * ONE).await?;
    let r = step_as(&w, &w.alice, b, 0).await?;
    assert!(r.logs().iter().any(|l| l.contains("step_failed")), "{:?}", r.logs());
    assert!(!r.logs().iter().any(|l| l.contains("decrease_collateral")), "collateral must not move: {:?}", r.logs());
    assert_eq!(balance(&w.wrap, &w.vault).await?, wrap_before);
    x(&w.alice, &w.vault, "drain", json!({"flow_id": b})).await?.into_result()?;

    // Repay 30 USDC and release 1/3 wNEAR per USDC: 10 wNEAR comes back to the vault.
    let wrap_before = balance(&w.wrap, &w.vault).await?;
    let back = save(&w, json!([{"input": w.usdc.id(), "action": repay_and_release(&w, &lend, NEAR / 3 * (SCALE / ONE)), "when": {"when": "manual"}}])).await?;
    fund(&w, back, 0, 30 * ONE).await?;
    let r = step_as(&w, &w.alice, back, 0).await?;
    assert!(r.logs().iter().any(|l| l.contains(r#""event":"step""#)), "{:?} {:?}", r.logs(), r.failures());
    let got = balance(&w.wrap, &w.vault).await? - wrap_before;
    println!("repaid 30 USDC, got {:.6} wNEAR back", got as f64 / NEAR as f64);
    assert!(got > 10 * NEAR - 1_000_000 && got <= 10 * NEAR, "{got}");
    let acct: Value = lend.view("get_account").args_json(json!({"account_id": &w.vault})).await?.json()?;
    println!("after repay: borrowed {}", acct["borrowed"]);
    Ok(())
}
