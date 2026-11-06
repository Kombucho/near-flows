//! Remote flow nodes: a flow whose nodes sign transactions on other chains.
//! The vault checks each trigger itself (price triggers from the oracle, here
//! mock_pyth with the real receiver's interface), puts the node's own input in
//! the transaction, signs it (mock_signer really signs), and routes the output
//! the keeper reports, but only between the minimum it signed and the node's cap.

mod common;
mod ladder;

use base64::Engine;
use common::wasm;
use ladder::*;
use near_workspaces::result::ExecutionFinalResult;
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
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "NVDA ladder", "nodes": ladder()})).max_gas().transact().await?.into_result()?.json()?;
    let eth_in = 30_000_000_000_000_000u128; // 0.03 ETH
    // The vault can't see its ETH on Robinhood Chain: the owner's word funds the entry node; nobody else's.
    assert!(w.keeper.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).transact().await?.is_failure());
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).max_gas().transact().await?.into_result()?;
    assert!(fails_with(&w.keeper.call(w.vault.id(), "step").args_json(json!({"flow_id": f, "node": 0})).max_gas().transact().await?, "step_remote"), "a remote node isn't a NEAR step");

    // Buy: the keeper asks for 999 ETH in; the vault signs the node's 0.03 ETH, a deadline 5 minutes out,
    // and the template's floor (10.67 NVDA per ETH) since the keeper's quote (1 wei) is lower.
    let r = step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 0)).await?;
    assert!(r.logs().iter().any(|l| l.contains("evm_signed")), "{:?} {:?}", r.logs(), r.failures());
    let req = event(&r, "evm_call_requested");
    assert_eq!(req["value"], json!(eth_in.to_string()), "the ETH attached is the node's input");
    let data = req["data"].as_str().unwrap().to_string();
    assert!(data.contains(&format!("{eth_in:064x}")) && !data.contains(&format!("{:064x}", 999 * E18)));
    let s = event(&r, "remote_step");
    let min = eth_in * 1067 / 100; // 0.32010 NVDA
    assert_eq!((num(&s["amount"]), num(&s["min_out"]), num(&s["max_out"])), (eth_in, min, eth_in * 20));
    let deadline_ns = s["expires_ns"].as_u64().unwrap();
    assert!(data.contains(&format!("{:064x}", deadline_ns / 1_000_000_000 - 1)), "the signed deadline");

    // While it's out there: the node can't run again, the flow can't be drained, and only the owner may call it failed.
    assert!(fails_with(&step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 1)).await?, "Busy"));
    assert!(w.alice.call(w.vault.id(), "drain").args_json(json!({"flow_id": f})).transact().await?.is_failure());
    assert!(fails_with(&report(&w, &w.keeper, f, 0, None).await?, "may still land"));
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
    let min1 = third * 738 / 10_000; // the template's floor: 0.0738 ETH per NVDA
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
async fn a_remote_step_that_did_not_land_gives_its_input_back_only_once_it_cannot_land() -> anyhow::Result<()> {
    let w = world().await?;
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "ladder", "nodes": ladder()})).max_gas().transact().await?.into_result()?.json()?;
    let eth_in = 10_000_000_000_000_000u128;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).max_gas().transact().await?.into_result()?;
    let r = step(&w, &w.keeper, f, 0, evm_args(v4_values(1, true), 0)).await?;
    let expires = event(&r, "remote_step")["expires_ns"].as_u64().unwrap();
    assert!(fails_with(&report(&w, &w.keeper, f, 0, None).await?, "may still land"));
    // Past the signed deadline the transaction can never land: the keeper may now say it failed.
    while w.worker.view_block().await?.timestamp() < expires {
        w.worker.fast_forward(100).await?;
    }
    let r = report(&w, &w.keeper, f, 0, None).await?;
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

#[tokio::test]
async fn a_remote_node_spends_only_what_its_template_spends_and_counts_against_limits() -> anyhow::Result<()> {
    let w = world().await?;
    // A node holding ETH wired to the NVDA-selling template: refused when it runs (the template spends NVDA).
    let mut bad = ladder();
    bad[1]["input"] = json!(tok("eth"));
    bad[1]["action"]["remote"]["out"] = json!(tok(NVDA));
    bad[1]["next"] = json!([]);
    bad[0]["next"] = json!([]);
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "bad", "nodes": [bad[0].clone(), bad[1].clone()]})).max_gas().transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 1, "amount": "1000"})).max_gas().transact().await?.into_result()?;
    nvda_price(&w, 500, 0).await?;
    assert!(fails_with(&step(&w, &w.keeper, f, 1, evm_args(v4_values(1, false), 0)).await?, "doesn't spend evm:4663:eth"));
    assert_eq!(inbox(&ledger(&w, f).await?, 1), 1000, "the refusal undid the ledger too");

    // Uncapped output can't even be saved.
    let mut uncapped = ladder();
    uncapped[0]["action"]["remote"]["max_out_per_in"] = Value::Null;
    uncapped[0]["next"] = json!([]);
    assert!(w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "x", "nodes": [uncapped[0].clone()]})).max_gas().transact().await?.is_failure());

    // A spending limit on ETH (0.015 per hour) holds across remote steps, whoever runs them.
    w.alice.call(w.vault.id(), "set_limit").args_json(json!({"token": "evm:4663:eth", "max": "15000000000000000", "window_ns": 3_600_000_000_000u64})).transact().await?.into_result()?;
    let f: u32 = w.alice.call(w.vault.id(), "save_flow").args_json(json!({"name": "ladder", "nodes": ladder()})).max_gas().transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": "20000000000000000"})).max_gas().transact().await?.into_result()?;
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
        prints.push(w.vault.view("fingerprint_of").args_json(json!({"node": n, "salt": s})).await?.json::<String>()?);
    }
    let inputs: Vec<Value> = nodes.iter().map(|n| n["input"].clone()).collect();
    let f: u32 = w.alice.call(w.vault.id(), "save_hidden_flow").args_json(json!({"name": "secret ladder", "inputs": inputs, "fingerprints": prints}))
        .transact().await?.into_result()?.json()?;
    w.alice.call(w.vault.id(), "fund").args_json(json!({"flow_id": f, "node": 0, "amount": "10000000000000000"})).max_gas().transact().await?.into_result()?;
    let run = |reveal: Value, salt: &str| {
        json!({"flow_id": f, "node": 0, "args": evm_args(v4_values(1, true), 0), "reveal": reveal, "salt": salt})
    };
    // A keeper swapping in a looser node (a 1000x cap) doesn't match the commitment.
    let mut greedy = nodes[0].clone();
    greedy["action"]["remote"]["max_out_per_in"] = json!((20_000 * E24).to_string());
    assert!(fails_with(&w.keeper.call(w.vault.id(), "step_remote").args_json(run(greedy, &salts[0])).max_gas().transact().await?, "fingerprint"));
    let r = w.keeper.call(w.vault.id(), "step_remote").args_json(run(nodes[0].clone(), &salts[0])).max_gas().transact().await?;
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
    let worker = near_workspaces::sandbox().await?;
    let (alice, keeper) = (worker.dev_create_account().await?, worker.dev_create_account().await?);
    let signer = worker.dev_deploy(&wasm("mock_signer")).await?;
    let vault = worker.dev_deploy(&wasm("vault")).await?;
    vault.call("new").args_json(json!({"owner": alice.id(), "keeper": keeper.id()})).transact().await?.into_result()?;
    alice.call(vault.id(), "sol_setup").args_json(json!({"signer": signer.id(), "max_cu_price": 100_000u64, "max_cu_limit": 1_400_000u32})).max_gas().transact().await?.into_result()?;
    let me_b58 = vault.view("get_sol").await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let me = solguard::pubkey(&me_b58);
    // Two templates: a fee claim paying the vault (spends nothing) and a buy spending at most 1 SOL, at least 1,000 coin units per lamport.
    alice.call(vault.id(), "sol_template").args_json(json!({"template": {"name": "claim_fee", "program": b58(&SWEEP_PROGRAM), "prefix": "01",
        "fields": [], "accounts": [{"acc": "vault"}], "spends": []}})).transact().await?.into_result()?;
    alice.call(vault.id(), "sol_template").args_json(json!({"template": {"name": "buy", "program": b58(&BUY_PROGRAM), "prefix": "02",
        "fields": [{"ty": "u64", "rule": "at_most", "max": "1000000000"}, {"ty": "u64", "rule": "at_least", "of": 0, "num": "1000", "den": "1"}],
        "accounts": [{"acc": "vault"}], "spends": [{"field": 0, "key": "sol:SOL"}]}})).transact().await?.into_result()?;
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
    let f: u32 = alice.call(vault.id(), "save_flow").args_json(json!({"name": "fees into a coin", "nodes": nodes})).max_gas().transact().await?.into_result()?.json()?;
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
        let (k, v) = (keeper.clone(), vault.id().clone());
        async move { k.call(&v, "step_remote").args_json(json!({"flow_id": f, "node": node, "args": args})).max_gas().transact().await }
    };
    let rep = |node: u16, out: u128| {
        let (k, v) = (keeper.clone(), vault.id().clone());
        async move { k.call(&v, "remote_report").args_json(json!({"flow_id": f, "node": node, "out": out.to_string()})).transact().await }
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
    let l: Value = vault.view("get_flow").args_json(json!({"flow_id": f})).await?.json::<Value>()?["ledger"].clone();
    assert_eq!((l["nodes"][1]["inbox"].as_str().unwrap(), l["nodes"][1]["out_total"].as_str().unwrap()), ("0", "160000000000"));
    Ok(())
}
