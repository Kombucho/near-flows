//! Remote flow nodes on a relay vault: a flow whose nodes sign transactions on
//! other chains. The brain checks each trigger itself (price triggers from the
//! oracle, here mock_pyth with the real receiver's interface), puts the node's
//! own input in the transaction, has the vault sign it (mock_signer really
//! signs), and routes the output the keeper reports, but only between the
//! minimum it signed and the node's cap.

mod common;
mod ladder;
mod relayw;

use base64::Engine;
use ladder::*;
use near_workspaces::result::ExecutionFinalResult;
use relayw::{pyth_feeds, relay_world, set_usd, view, x, ETH_FEED, USDC_FEED};
use serde_json::{json, Value};

fn num(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

fn fails_with(r: &ExecutionFinalResult, why: &str) -> bool {
    r.is_failure() && format!("{:?}", r.failures()).contains(why)
}

#[tokio::test]
async fn a_take_profit_ladder_fires_on_oracle_prices_and_routes_only_bounded_reports() -> anyhow::Result<()> {
    let w = world().await?;
    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "NVDA ladder", "nodes": ladder()})).await?.into_result()?.json()?;
    let eth_in = 30_000_000_000_000_000u128; // 0.03 ETH
    // The vault can't see its ETH on Robinhood Chain: the owner's word funds the entry node; nobody else's.
    assert!(req(&w, &w.keeper, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.is_failure());
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    assert!(fails_with(&req(&w, &w.keeper, "step", json!({"flow_id": f, "node": 0})).await?, "step_remote"), "a remote node isn't a NEAR step");

    // Buy: the keeper asks for 999 ETH in; the vault signs the node's 0.03 ETH, a deadline 5 minutes out,
    // and the template's floor (10.67 NVDA per ETH) since the keeper's quote (1 wei) is lower.
    let r = step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 0)).await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    let call = event(&r, "evm_call_requested");
    assert_eq!(call["value"], json!(eth_in.to_string()), "the ETH attached is the node's input");
    let data = call["data"].as_str().unwrap().to_string();
    assert!(data.contains(&format!("{eth_in:064x}")) && !data.contains(&format!("{:064x}", 999 * E18)));
    let s = event(&r, "remote_step");
    let min = eth_in * 1067 / 100; // 0.32010 NVDA
    assert_eq!((num(&s["amount"]), num(&s["min_out"]), num(&s["max_out"])), (eth_in, min, eth_in * 20));
    let deadline_ns = s["expires_ns"].as_u64().unwrap();
    assert!(data.contains(&format!("{:064x}", deadline_ns / 1_000_000_000 - 1)), "the signed deadline");

    // While it's out there: the node can't run again, the flow can't be drained, and only the owner may call it failed.
    assert!(fails_with(&step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 1)).await?, "Busy"));
    assert!(req(&w, &w.alice, "drain", json!({"flow_id": f})).await?.is_failure());
    assert!(fails_with(&report(&w, &w.keeper, f, 0, None).await?, "only the owner may say"));
    // Reports outside the bounds: less than the signed minimum (that tx reverted), more than the cap.
    assert!(fails_with(&report(&w, &w.keeper, f, 0, Some(min - 1)).await?, "below the signed minimum"));
    assert!(fails_with(&report(&w, &w.keeper, f, 0, Some(eth_in * 20 + 1)).await?, "over the node's cap"));
    // Landed: 0.3552 NVDA arrived. A third each to the rungs (3,333 bps), the rest stays free.
    let got = 355_200_000_000_000_000u128;
    let r = report(&w, &w.keeper, f, 0, Some(got)).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let l = ledger(&w, f).await?;
    let third = got * 3_333 / 10_000;
    assert_eq!((inbox(&l, 0), inbox(&l, 1), inbox(&l, 2), inbox(&l, 3)), (0, third, third, third));
    assert_eq!(third, 118_388_160_000_000_000);
    assert!(fails_with(&report(&w, &w.keeper, f, 0, Some(got)).await?, "no remote step pending"), "reported once");

    // NVDA at $200: under every rung. The price is the oracle's: no keeper number goes in.
    nvda_price(&w, 200, 0).await?;
    for rung in 1..=3u16 {
        assert!(fails_with(&step(&w, &w.keeper, f, rung, evm_args(v4_values(1, false), 1)).await?, "NotYet"), "rung {rung}");
    }
    // $275 (>= 1.5 x 180 = 270): the first rung only. The price it saw is in the log, scaled 1e24.
    nvda_price(&w, 275, 0).await?;
    let r = step(&w, &w.keeper, f, 1, evm_args(v4_values(1, false), 1)).await?;
    assert_eq!(num(&event(&r, "remote_price")["price"]), 275 * E24);
    let s = event(&r, "remote_step");
    let min1 = third * 738 / 10_000; // the template's floor: 0.0738 ETH per NVDA (NVDA is single-source: no market floor)
    assert_eq!((num(&s["amount"]), num(&s["min_out"]), num(&s["max_out"])), (third, min1, third / 5));
    // One step in flight per flow: the other rungs wait for its report.
    assert!(fails_with(&step(&w, &w.keeper, f, 2, evm_args(v4_values(1, false), 2)).await?, "Busy"));
    let eth1 = 9_700_000_000_000_000u128; // 0.0097 ETH
    report(&w, &w.keeper, f, 1, Some(eth1)).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, f, 2, evm_args(v4_values(1, false), 2)).await?, "NotYet"), "$275 < 2 x 180");
    assert_eq!(inbox(&ledger(&w, f).await?, 4), eth1, "the sale's ETH waits at the basket node");

    // A stale price (2 minutes old) is no price: the next rung can't fire on it even at $400.
    nvda_price(&w, 400, 120).await?;
    let r = step(&w, &w.keeper, f, 2, evm_args(v4_values(1, false), 2)).await?;
    assert!(fails_with(&r, "no price fresher"), "{:?}", r.failures());
    assert_eq!(inbox(&ledger(&w, f).await?, 2), third, "nothing taken");

    // The basket node: 0.0097 ETH zapped into ETHNVDA, at least 1.536 shares per ETH.
    let r = step(&w, &w.keeper, f, 4, evm_args(vec!["0".into(), "0".into(), "0".into(), "0".into(), "0".into()], 2)).await?;
    let s = event(&r, "remote_step");
    assert_eq!((num(&s["amount"]), num(&s["min_out"])), (eth1, eth1 * 1536 / 1000));
    assert_eq!(event(&r, "evm_call_requested")["value"], json!(eth1.to_string()));
    report(&w, &w.keeper, f, 4, Some(eth1 * 17 / 10)).await?.into_result()?;
    let l = ledger(&w, f).await?;
    assert_eq!(num(&l["nodes"][4]["out_total"]), eth1 * 17 / 10);
    Ok(())
}

#[tokio::test]
async fn a_remote_step_that_did_not_land_gives_its_input_back_only_on_the_owners_word() -> anyhow::Result<()> {
    let w = world().await?;
    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "ladder", "nodes": ladder()})).await?.into_result()?.json()?;
    let eth_in = 10_000_000_000_000_000u128;
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 0)).await?;
    let expires = event(&r, "remote_step")["expires_ns"].as_u64().unwrap();
    // The double spend: the buy LANDED (before its deadline), then the keeper says it failed. Its 0.01 ETH
    // would go back to the inbox and be signed away again: ETH the vault no longer has for this flow
    // (another flow's, or the owner's). Nothing here can see Robinhood Chain: refused before and after the deadline.
    assert!(fails_with(&report(&w, &w.keeper, f, 0, None).await?, "only the owner may say"));
    while w.worker.view_block().await?.timestamp() < expires {
        w.worker.fast_forward(100).await?;
    }
    assert!(fails_with(&report(&w, &w.keeper, f, 0, None).await?, "only the owner may say"), "past the deadline too");
    assert_eq!(inbox(&ledger(&w, f).await?, 0), 0, "the input stays out");
    // The keeper's honest path is the report of what arrived (bounded); the owner, who can look, says it failed.
    let r = report(&w, &w.alice, f, 0, None).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(num(&event(&r, "step_failed")["returned"]), eth_in);
    let l = ledger(&w, f).await?;
    assert_eq!((inbox(&l, 0), l["nodes"][0]["failures"].as_u64().unwrap()), (eth_in, 1));

    // The owner may settle at any time (e.g. after replacing the transaction's nonce).
    step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 1)).await?.into_result()?;
    report(&w, &w.alice, f, 0, None).await?.into_result()?;
    assert_eq!(inbox(&ledger(&w, f).await?, 0), eth_in);
    // Three failures in a row pause the node, like any node.
    step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 2)).await?.into_result()?;
    report(&w, &w.alice, f, 0, None).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 3)).await?, "Paused"));
    Ok(())
}

/// What a keeper could pick to make a signed swap land later, when its minimum is stale (or never,
/// holding every later step): a far deadline, a nonce ahead, a fee under the base fee. The brain picks all three.
#[tokio::test]
async fn a_keeper_cannot_pick_the_deadline_the_nonce_or_the_fee() -> anyhow::Result<()> {
    let w = world().await?;
    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "ladder", "nodes": ladder()})).await?.into_result()?.json()?;
    let eth_in = 10_000_000_000_000_000u128;
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    let mut v = v4_values(1, true);
    v[15] = u64::MAX.to_string(); // a deadline that never comes
    let args = |nonce: u64| json!({"chain": "evm", "values": v.clone(), "nonce": nonce, "max_fee": "1", "max_tip": "0", "gas": 1_000_000u64});
    assert!(fails_with(&step(&w, &w.keeper, f, 0, args(5)).await?, "refused: nonce 5"), "signing ahead, to land later");
    let r = step(&w, &w.keeper, f, 0, args(0)).await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    // The deadline is the brain's: 5 minutes from the block.
    let deadline = event(&r, "remote_step")["expires_ns"].as_u64().unwrap() / 1_000_000_000 - 1;
    let block_s = w.worker.view_block().await?.timestamp() / 1_000_000_000;
    assert!(deadline <= block_s + 300 && deadline + 60 >= block_s + 300, "deadline {deadline}, block {block_s}");
    let data = event(&r, "evm_call_requested")["data"].as_str().unwrap().to_string();
    assert!(data.contains(&format!("{deadline:064x}")) && !data.contains(&format!("{:064x}", u64::MAX)), "the signed deadline");
    // The fee is the owner's cap (1 gwei), not the keeper's 1 wei that would never get mined.
    let tx = evmtx::Eip1559 { chain_id: 4663, nonce: 0, max_priority_fee: 0, max_fee: GWEI, gas_limit: 1_000_000,
        to: evmtx::parse_address("0x8876789976decbfcbbbe364623c63652db8c0904").unwrap(), value: eth_in, data: hex::decode(data.trim_start_matches("0x"))? };
    assert_eq!(event(&r, "evm_signed")["hash"], json!(format!("0x{}", hex::encode(tx.signing_hash()))), "signed at the owner's fee cap");

    // Nonce 0 is used: signing it again (a second transaction for the same nonce) is refused, the next one signs.
    report(&w, &w.alice, f, 0, None).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, f, 0, args(0)).await?, "refused: nonce 0"), "a used nonce");
    step(&w, &w.keeper, f, 0, args(1)).await?.into_result()?;
    report(&w, &w.alice, f, 0, None).await?.into_result()?;
    // The owner may sign at any nonce (to unblock or replace); the keeper follows from there.
    let approve = json!({"name": "approve_nvda_permit2", "values": ["0", E18.to_string()], "nonce": 9, "max_fee": GWEI.to_string(), "max_tip": "0", "gas": 200_000u64});
    req(&w, &w.alice, "evm_call", approve).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, f, 0, args(2)).await?, "next EVM nonce is 10"));
    step(&w, &w.keeper, f, 0, args(10)).await?.into_result()?;
    report(&w, &w.keeper, f, 0, Some(eth_in * 11)).await?.into_result()?;
    assert_eq!(view(&w.brain, &w.vault, "get_evm", Value::Null).await?["next_nonce"], json!(11));
    Ok(())
}

/// The library's Base WETH -> USDC swap has a market minimum (ETH and USDC each have an independent
/// backup): a keeper running its node can't get it signed at the fixed floor (2,000 USDC per WETH,
/// ~80% of a $2,500 market), the gap a sandwich would take.
#[tokio::test]
async fn a_library_swap_with_a_market_leaf_is_signed_at_the_market_not_at_its_fixed_floor() -> anyhow::Result<()> {
    const WETH: &str = "0x4200000000000000000000000000000000000006";
    const USDC: &str = "0x833589fcd6edb6e08f4c7c32d4f71b54bda02913";
    let w = relay_world().await?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 8453u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 500_000u64})).await?.into_result()?;
    for t in common::templates("base") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    let pyth = pyth_feeds(&w.worker, &w.alice, &w.vault, &[("eth", ETH_FEED), ("usdc", USDC_FEED)]).await?;
    let prices = |eth_usd: i64, age_s: i64| {
        let (a, p) = (w.alice.clone(), pyth.clone());
        async move {
            set_usd(&a, &p, ETH_FEED, eth_usd * 100_000_000, age_s).await?;
            set_usd(&a, &p, USDC_FEED, 100_000_000, 0).await
        }
    };
    prices(2_500, 0).await?;
    // One node: sell the WETH it holds for USDC, at most 3,000 USDC per WETH may be reported.
    let node = json!({"input": format!("evm:8453:{WETH}"), "when": {"when": "now"}, "next": [],
        "action": {"label": "WETH to USDC", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "evm", "template": "uniswap_swap_weth_usdc", "amount_leaf": 4, "min_leaf": 5},
                "out": format!("evm:8453:{USDC}"), "max_out_per_in": "3000000000000000"}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "sell", "nodes": [node]})).await?.into_result()?.json()?;
    let weth_in = 100_000_000_000_000_000u128; // 0.1 WETH
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": weth_in.to_string()})).await?.into_result()?;
    let args = |nonce: u64| json!({"chain": "evm", "values": ["0", "0", "0", "0", "0", "1", "0"], "nonce": nonce, "max_fee": GWEI.to_string(), "max_tip": "0", "gas": 400_000u64});
    let run = || x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": f, "node": 0, "args": args(0)}));
    // A stale ETH price is no price: nothing signed, nothing taken.
    prices(2_500, 120).await?;
    assert!(fails_with(&run().await?, "no price fresher"));
    prices(2_500, 0).await?;
    // The keeper quotes 1 wei; the fixed floor says 200 USDC; the market says $2,500 x 0.1 less 1% = 247.5 USDC.
    let r = run().await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    let s = event(&r, "remote_step");
    let market = weth_in * 2_500 * 9_900 / 10_000 / 1_000_000_000_000;
    assert_eq!((num(&s["amount"]), num(&s["min_out"])), (weth_in, market));
    assert_eq!(market, 247_500_000);
    assert!(event(&r, "evm_call_requested")["data"].as_str().unwrap().contains(&format!("{market:064x}")), "the market minimum is in the signed swap");
    // A fill at the fixed floor means the signed swap reverted: such a report is refused.
    assert!(fails_with(&report_on(&w.keeper, &w.vault, f, Some(200_000_000)).await?, "below the signed minimum"));
    report_on(&w.keeper, &w.vault, f, Some(249_000_000)).await?.into_result()?;
    Ok(())
}

async fn report_on(who: &near_workspaces::Account, vault: &near_workspaces::AccountId, f: u32, out: Option<u128>) -> anyhow::Result<ExecutionFinalResult> {
    x(who, vault, "remote_report", json!({"flow_id": f, "node": 0, "out": out.map(|o| o.to_string())})).await
}

#[tokio::test]
async fn a_remote_node_spends_only_what_its_template_spends_and_counts_against_limits() -> anyhow::Result<()> {
    let w = world().await?;
    // A node holding ETH wired to the NVDA-selling template: refused when it runs (the template spends NVDA).
    let mut bad = ladder();
    bad[1]["input"] = json!(tok("eth"));
    bad[1]["action"]["remote"]["out"] = json!(tok(NVDA));
    bad[1]["next"] = json!([]);
    bad[0]["next"] = json!([]);
    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "bad", "nodes": [bad[0].clone(), bad[1].clone()]})).await?.into_result()?.json()?;
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 1, "amount": "1000"})).await?.into_result()?;
    nvda_price(&w, 500, 0).await?;
    assert!(fails_with(&step(&w, &w.keeper, f, 1, evm_args(v4_values(1, false), 0)).await?, "doesn't spend evm:4663:eth"));
    assert_eq!(inbox(&ledger(&w, f).await?, 1), 1000, "the refusal undid the ledger too");

    // Uncapped output can't even be saved.
    let mut uncapped = ladder();
    uncapped[0]["action"]["remote"]["max_out_per_in"] = Value::Null;
    uncapped[0]["next"] = json!([]);
    assert!(req(&w, &w.alice, "save_flow", json!({"name": "x", "nodes": [uncapped[0].clone()]})).await?.is_failure());

    // A spending limit on ETH (0.015 per hour) holds across remote steps, whoever runs them.
    req(&w, &w.alice, "set_limit", json!({"token": "evm:4663:eth", "max": "15000000000000000", "window_ns": 3_600_000_000_000u64})).await?.into_result()?;
    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "ladder", "nodes": ladder()})).await?.into_result()?.json()?;
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": "20000000000000000"})).await?.into_result()?;
    assert!(fails_with(&step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 0)).await?, "spending limit"));
    assert_eq!(inbox(&ledger(&w, f).await?, 0), 20_000_000_000_000_000);
    Ok(())
}

#[tokio::test]
async fn a_hidden_remote_node_runs_only_as_committed() -> anyhow::Result<()> {
    let w = world().await?;
    let nodes = ladder();
    let salts: Vec<String> = (0..nodes.len()).map(|i| format!("{:02x}", i + 1).repeat(32)).collect();
    let mut prints = Vec::new();
    for (n, s) in nodes.iter().zip(&salts) {
        prints.push(w.brain.view("fingerprint_of").args_json(json!({"node": n, "salt": s})).await?.json::<String>()?);
    }
    let inputs: Vec<Value> = nodes.iter().map(|n| n["input"].clone()).collect();
    let f: u32 = req(&w, &w.alice, "save_hidden_flow", json!({"name": "secret ladder", "inputs": inputs, "fingerprints": prints}))
        .await?.into_result()?.json()?;
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": "10000000000000000"})).await?.into_result()?;
    let run = |reveal: Value, salt: &str| {
        json!({"flow_id": f, "node": 0, "args": evm_args(v4_values(1, true), 0), "reveal": reveal, "salt": salt})
    };
    // A keeper swapping in a looser node (a 1000x cap) doesn't match the commitment.
    let mut greedy = nodes[0].clone();
    greedy["action"]["remote"]["max_out_per_in"] = json!((20_000 * E24).to_string());
    assert!(fails_with(&req(&w, &w.keeper, "step_remote", run(greedy, &salts[0])).await?, "fingerprint"));
    let r = req(&w, &w.keeper, "step_remote", run(nodes[0].clone(), &salts[0])).await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?}", r.failures());
    // The report routes along the revealed node's routes.
    report(&w, &w.keeper, f, 0, Some(150_000_000_000_000_000)).await?.into_result()?;
    let l = ledger(&w, f).await?;
    assert_eq!(inbox(&l, 1), 150_000_000_000_000_000 * 3_333 / 10_000);
    Ok(())
}

// ---------- Solana: LP fees swept into a memecoin buy ----------

const SWEEP_PROGRAM: [u8; 32] = [11; 32]; // stands in for an LP program's claim_fee
const BUY_PROGRAM: [u8; 32] = [12; 32]; // stands in for pump.fun's buy_exact_sol_in

fn b58(k: &[u8; 32]) -> String {
    bs58::encode(k).into_string()
}

#[tokio::test]
async fn solana_lp_fees_are_swept_into_a_memecoin_buy() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let (alice, keeper, vault) = (&w.alice, &w.keeper, &w.vault);
    x(alice, vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 100_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me_b58 = view(&w.brain, vault, "get_sol", Value::Null).await?["address"].as_str().unwrap().to_string();
    let me = solguard::pubkey(&me_b58);
    // Two templates: a fee claim paying the vault (spends nothing) and a buy spending at most 1 SOL, at least 1,000 coin units per lamport.
    x(alice, vault, "sol_template", json!({"template": {"name": "claim_fee", "program": b58(&SWEEP_PROGRAM), "prefix": "01",
        "fields": [], "accounts": [{"acc": "vault"}], "spends": []}})).await?.into_result()?;
    x(alice, vault, "sol_template", json!({"template": {"name": "buy", "program": b58(&BUY_PROGRAM), "prefix": "02",
        "fields": [{"ty": "u64", "rule": "at_most", "max": "1000000000"}, {"ty": "u64", "rule": "at_least", "of": 0, "num": "1000", "den": "1"}],
        "accounts": [{"acc": "vault"}], "spends": [{"field": 0, "key": "sol:SOL"}]}})).await?.into_result()?;
    let coin = "sol:Coin1111111111111111111111111111111111111";
    let nodes = json!([
        // 0: sweep fees every hour; whatever is reported (at most 0.5 SOL a sweep) goes to the buy.
        {"input": "none", "when": {"when": "every", "every_ns": 3_600_000_000_000u64, "chunk": "1"}, "next": [{"to": 1, "bps": 10_000}],
         "action": {"label": "Sweep LP fees", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "claim_fee"}, "out": "sol:SOL", "max_out": "500000000"}}},
        // 1: buy the coin with exactly what the sweep brought.
        {"input": "sol:SOL", "when": {"when": "now"}, "next": [],
         "action": {"label": "Buy the memecoin", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "buy", "amount_field": 0, "min_field": 1}, "out": coin, "max_out_per_in": (100_000 * E24).to_string()}}}
    ]);
    let f: u32 = x(alice, vault, "save_flow", json!({"name": "fees into a coin", "nodes": nodes})).await?.into_result()?.json()?;
    let b64 = |b: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(b);
    let claim = || solguard::testkit::Tx::new(me).ix(SWEEP_PROGRAM, vec![me], vec![1]).bytes();
    let buy = |lamports: u64, min: u64| {
        let mut d = vec![2];
        d.extend(lamports.to_le_bytes());
        d.extend(min.to_le_bytes());
        solguard::testkit::Tx::new(me).ix(BUY_PROGRAM, vec![me], d).bytes()
    };
    let sol = |msg: Vec<u8>| json!({"chain": "sol", "message": b64(msg)});
    let run = |node: u16, args: Value| {
        let (k, v) = (keeper.clone(), vault.clone());
        async move { x(&k, &v, "step_remote", json!({"flow_id": f, "node": node, "args": args})).await }
    };
    let rep = |node: u16, out: u128| {
        let (k, v) = (keeper.clone(), vault.clone());
        async move { x(&k, &v, "remote_report", json!({"flow_id": f, "node": node, "out": out.to_string()})).await }
    };

    // The sweep: a claim with a buy riding along (it spends) is refused; the plain claim is signed.
    let mut stowaway = vec![2];
    stowaway.extend(5u64.to_le_bytes());
    stowaway.extend(5000u64.to_le_bytes());
    let both = solguard::testkit::Tx::new(me).ix(SWEEP_PROGRAM, vec![me], vec![1]).ix(BUY_PROGRAM, vec![me], stowaway).bytes();
    let r = run(0, sol(both)).await?;
    assert!(fails_with(&r, "rides along and spends"), "{:?}", r.failures());
    let r = run(0, sol(claim())).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?} {:?}", r.logs(), r.failures());
    let s = event(&r, "remote_step");
    assert_eq!((num(&s["amount"]), num(&s["min_out"]), num(&s["max_out"])), (0, 0, 500_000_000));
    assert!(rep(0, 500_000_001).await?.is_failure(), "over the sweep's cap");
    rep(0, 120_000_000).await?.into_result()?; // 0.12 SOL of fees
    assert!(run(0, sol(claim())).await?.is_failure(), "the next sweep waits an hour");

    // The buy must spend exactly the 0.12 SOL the node holds: not more, not less.
    for (lamports, why) in [(130_000_000u64, "exactly"), (110_000_000, "exactly")] {
        assert!(fails_with(&run(1, sol(buy(lamports, lamports * 1000))).await?, why));
    }
    let r = run(1, sol(buy(120_000_000, 150_000_000_000))).await?;
    assert!(r.logs().iter().any(|l| l.contains("sol_signed")), "{:?}", r.failures());
    let s = event(&r, "remote_step");
    // The signed minimum is the message's own (150e9 coin units, over the template's 1,000 per lamport = 120e9).
    assert_eq!((num(&s["amount"]), num(&s["min_out"])), (120_000_000, 150_000_000_000));
    assert!(rep(1, 149_999_999_999).await?.is_failure());
    rep(1, 160_000_000_000).await?.into_result()?;
    let l = view(&w.brain, vault, "get_flow", json!({"flow_id": f})).await?["ledger"].clone();
    assert_eq!((l["nodes"][1]["inbox"].as_str().unwrap(), l["nodes"][1]["out_total"].as_str().unwrap()), ("0", "160000000000"));
    Ok(())
}
