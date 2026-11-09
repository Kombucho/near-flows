//! The take-profit ladder end to end on a local fork of Robinhood Chain mainnet:
//! a NEAR flow buys NVDA with ETH (Uniswap v4), sells a third at each of 1.5x /
//! 2x / 3x of $180 as the oracle (mock_pyth) walks the price up, and zaps every
//! sale's ETH into StonkBrokers' ETHNVDA pair token. Every step has a market floor:
//! the swaps' from the ETH and NVDA feeds (each with its backup), the zap's from the
//! share's value at those feeds and the k the owner pins from the fork (2 L / S).
//! ETH walks up with NVDA at the fork pool's own ratio, so the floors are the market
//! the fork trades at (a pool that doesn't follow the oracle would be refused). The relay vault's brain decides each step
//! (trigger, amount, minimum, deadline) and signs; the keeper broadcasts on the
//! fork and reports what arrived, which the vault accepts only within bounds.
//! At the end the fork's balances must equal what the flow's ledger says.
//!
//! Needs Foundry (anvil, cast); skipped without it. ROBINHOOD_RPC overrides the public RPC.

mod common;
mod ladder;
mod relayw;

use common::{num, signed, Fork};
use ladder::*;
use serde_json::{json, Value};

const PORT: u16 = 9802;

#[tokio::test]
async fn a_nvda_take_profit_ladder_walks_up_on_a_robinhood_fork_into_the_ethnvda_basket() -> anyhow::Result<()> {
    let url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    let Some(fork) = Fork::start(&url, PORT).await else { return Ok(()) };
    assert_eq!(fork.cast(&["chain-id"]), "4663", "a fork of Robinhood Chain");
    let ratio = pool_ratio_e6(&fork);
    let w = world_at(ratio).await?;
    let me = evm_address(&w).await?;
    // The owner pins the zap's k from the fork (the library's was read on 2026-11-09).
    let k = fork_k_pair(&fork);
    let zap_t = crate::common::templates("robinhood").into_iter().find(|t| t["name"] == "pairtoken_ethnvda_zap_eth").unwrap();
    req(&w, &w.alice, "evm_template", json!({"template": with_k(&zap_t, k)})).await?.into_result()?;
    println!("pool mid {:.4} NVDA per ETH (ETH follows NVDA at it); ETHNVDA k {k} (library {})", ratio as f64 / 1e6, pinned_k("pairtoken_ethnvda_zap_eth"));
    println!("Robinhood fork at block {}, vault's address {me}", fork.cast(&["block-number"]));
    fork.cast(&["rpc", "anvil_setBalance", &me, "0xDE0B6B3A7640000"]); // 1 ETH
    let eth_of = |f: &Fork| num(&f.cast(&["balance", &me]));
    let max_fee = (num(&fork.cast(&["base-fee"])) * 2).max(GWEI / 100).min(GWEI);
    let nonce = |f: &Fork| num(&f.cast(&["nonce", &me])) as u64;
    let args = |values: Vec<String>, nonce: u64| json!({"chain": "evm", "values": values, "nonce": nonce, "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 1_000_000u64});

    // Once: let the Universal Router pull NVDA through Permit2 (plain template calls, outside the flow).
    let now: u128 = num(&fork.cast(&["block", "latest", "--field", "timestamp"]));
    for (name, values) in [("approve_nvda_permit2", vec![0, E18]), ("permit2_nvda_universal_router", vec![0, 0, E18, now + 86_400])] {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce(&fork), "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 200_000u64});
        let r = req(&w, &w.alice, "evm_call", a).await?;
        assert_eq!(send(&fork, &signed(&r)).0, 1, "{name}");
    }

    let f: u32 = req(&w, &w.alice, "save_flow", json!({"name": "NVDA ladder", "nodes": ladder()})).await?.into_result()?.json()?;
    let eth_in = 30_000_000_000_000_000u128; // 0.03 ETH
    req(&w, &w.alice, "fund", json!({"flow_id": f, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;

    // 1. Buy (trigger: now). The keeper quotes 99% of the pool's own quote; the vault signs at least that or its floor.
    let q = v4_quote(&fork, true, eth_in);
    let r = step(&w, &w.keeper, f, 0, args(v4_values(q * 99 / 100, true), nonce(&fork))).await?;
    let floor = market_floor(eth_in, eth_e8(&w, 180), 180 * 100_000_000, 300);
    assert!(num_s(&event(&r, "remote_step")["min_out"]) >= floor, "at least the market floor");
    let (st, _) = send(&fork, &signed(&r));
    assert_eq!(st, 1, "buy executed");
    let bought = fork.balance(NVDA, &me);
    println!("  market floor {:.6} NVDA (ETH ${:.2}, NVDA $180, less 3%)", floor as f64 / 1e18, eth_e8(&w, 180) as f64 / 1e8);
    println!("buy: 0.03 ETH -> {:.6} NVDA (quote {:.6}, signed minimum {:.6})", bought as f64 / 1e18, q as f64 / 1e18, num_s(&event(&r, "remote_step")["min_out"]) as f64 / 1e18);
    report(&w, &w.keeper, f, 0, Some(bought)).await?.into_result()?;
    let third = bought * 3_333 / 10_000;
    assert_eq!(inbox(&ledger(&w, f).await?, 1), third);

    // 2. Walk the price up: each rung sells its third when the oracle reaches it, then the basket takes the ETH.
    let mut basket_shares = 0u128;
    let mut eth_from_sales = 0u128;
    for (rung, usd) in [(1u16, 275i64), (2, 365), (3, 545)] {
        // Just below this rung: refused, nothing moves.
        nvda_price(&w, 180 * [15, 20, 30][rung as usize - 1] / 10 - 1, 0).await?;
        let r = step(&w, &w.keeper, f, rung, args(v4_values(1, false), nonce(&fork))).await?;
        assert!(format!("{:?}", r.failures()).contains("NotYet"), "rung {rung} below its price");
        // The pool is thin (~$5.5k at full range): our own trades moved it. A real chain's arbitrage
        // would pull it back to the market; the fork has none, so the market (ETH's price) follows the pool.
        w.ratio_e6.set(pool_ratio_e6(&fork));
        nvda_price(&w, usd, 0).await?;
        let q = v4_quote(&fork, false, third);
        let before = eth_of(&fork);
        let r = step(&w, &w.keeper, f, rung, args(v4_values(q * 99 / 100, false), nonce(&fork))).await?;
        let floor = market_floor(third, usd * 100_000_000, eth_e8(&w, usd), 300).max(third * 738 / 10_000);
        assert_eq!(num_s(&event(&r, "remote_step")["min_out"]), floor.max(q * 99 / 100), "the higher of the keeper's quote and the floors");
        let (st, gas) = send(&fork, &signed(&r));
        assert_eq!(st, 1, "rung {rung} sale executed");
        let got = eth_of(&fork) + gas - before; // what the swap paid, gas added back
        println!("NVDA at ${usd}: rung {rung} sold {:.6} NVDA -> {:.6} ETH (quote {:.6})", third as f64 / 1e18, got as f64 / 1e18, q as f64 / 1e18);
        assert!(got >= q * 99 / 100);
        report(&w, &w.keeper, f, rung, Some(got)).await?.into_result()?;
        eth_from_sales += got;

        // The basket: zap exactly that ETH into ETHNVDA (the node's input; the keeper's 0 is ignored).
        w.ratio_e6.set(pool_ratio_e6(&fork));
        nvda_price(&w, usd, 0).await?;
        let shares_before = fork.balance(ETHNVDA, &me);
        let r = step(&w, &w.keeper, f, 4, args(vec!["0".into(); 5], nonce(&fork))).await?;
        assert_eq!(event(&r, "evm_call_requested")["value"], json!(got.to_string()));
        let min = num_s(&event(&r, "remote_step")["min_out"]);
        let want = zap_floor(got, eth_e8(&w, usd), usd * 100_000_000, k, 300);
        assert!((min as f64 / want - 1.0).abs() < 1e-9, "the share floor, not the keeper's 0: {min} vs {want}");
        assert_eq!(send(&fork, &signed(&r)).0, 1, "zap executed");
        let shares = fork.balance(ETHNVDA, &me) - shares_before;
        println!("  basket: {:.6} ETH -> {:.6} ETHNVDA shares (share floor {:.6})", got as f64 / 1e18, shares as f64 / 1e18, min as f64 / 1e18);
        assert!(shares >= min);
        report(&w, &w.keeper, f, 4, Some(shares)).await?.into_result()?;
        basket_shares += shares;
    }

    // 3. The fork agrees with the ledger: every rung empty, the basket holds what the basket node reported,
    //    and the NVDA left at the vault's address is exactly the unrouted remainder (1 bp of the buy).
    let l = ledger(&w, f).await?;
    for i in 0..5 {
        assert_eq!(inbox(&l, i), 0, "node {i} empty");
    }
    assert_eq!(fork.balance(ETHNVDA, &me), basket_shares);
    assert_eq!(num_s(&l["nodes"][4]["out_total"]), basket_shares);
    assert_eq!(fork.balance(NVDA, &me), bought - 3 * third);
    println!("ladder done: 0.03 ETH -> {:.6} NVDA -> {:.6} ETH in three sales -> {:.6} ETHNVDA shares; {} NVDA wei left unrouted",
        bought as f64 / 1e18, eth_from_sales as f64 / 1e18, basket_shares as f64 / 1e18, bought - 3 * third);
    Ok(())
}

fn num_s(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}
