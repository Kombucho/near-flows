//! StonkBrokers on Robinhood Chain, from a NEAR relay vault: the node library's templates
//! for Smart LP (deposit USDG into the audited sdAMZN-FR-USDG vault, withdraw), Uniswap v4
//! through the Universal Router (ETH -> NVDA -> ETH, Permit2 approvals) and the ETHNVDA pair
//! token (zap ETH in, redeem to ETH), executed on a local fork of Robinhood Chain mainnet
//! (anvil on port 9801, ANVIL_PORT overrides). The brain builds and checks each call, the
//! vault signs it with its own MPC path; what comes back lands at the vault's own EVM address,
//! the only address a template can name.
//!
//! Every minimum follows the market, none is the caller's: the v4 swaps' from the ETH and
//! NVDA feeds, the zaps' from the share's value (its two tokens' feeds and the k the owner
//! pins from the chain: 2 L / S for the pair token, total value / supply for Smart LP). The
//! test asks for 0 or 1 every time and gets the floor; a pool off the market is refused.
//! Feeds: mock_pyth, set from the fork's own Chainlink answers (RHNVDA, AMZN) and ETH at the
//! v4 pool's mid ratio.
//!
//! Needs Foundry (anvil, cast); skipped without it. ROBINHOOD_RPC overrides the public RPC.

mod common;
mod ladder;
mod relayw;

use common::{num, signed, wasm, Fork};
use ladder::{chainlink_e8, fork_k_pair, fork_k_smartlp, market_floor, pinned_k, pool_ratio_e6, set_pair, v4_quote, with_k, zap_floor, AMZN_CHAINLINK, ETHNVDA,
    NVDA, RHNVDA_FEED, SMART_LP};
use relayw::{relay_world, view, x, RelayWorld};
use serde_json::{json, Value};

const USDG: &str = "0x5fc5360d0400a0fd4f2af552add042d716f1d168";
const AMZN_POOL: &str = "0x8ac92da74ab5f3b1d024dc1943ad7e15dc4179ef"; // holds USDG: funds the vault on the fork
const AMZN_FEED: &str = "b5d0e0fa58a1f8b81498ae670ce93c872d14434b72c364885d4fa1b257cbb07a";
const GWEI: u128 = 1_000_000_000;
const E18: u128 = 1_000_000_000_000_000_000;

fn leaf_min(r: &near_workspaces::result::ExecutionFinalResult, at: usize) -> u128 {
    // evm_call_requested's data: selector, then words; the minimum's word index `at`.
    let log = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap().to_string();
    let v: Value = serde_json::from_str(log.trim_start_matches("EVENT_JSON:")).unwrap();
    let d = v["data"][0]["data"].as_str().unwrap().trim_start_matches("0x").to_string();
    u128::from_str_radix(&d[8 + at * 64 + 32..8 + (at + 1) * 64], 16).unwrap()
}

#[tokio::test]
async fn the_vault_runs_stonkbrokers_smart_lp_uniswap_v4_and_pair_tokens_on_a_robinhood_fork() -> anyhow::Result<()> {
    let url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    let Some(fork) = Fork::start(&url, 9801).await else { return Ok(()) };
    assert_eq!(fork.cast(&["chain-id"]), "4663", "a fork of Robinhood Chain");
    println!("Robinhood Chain fork at block {}", fork.cast(&["block-number"]));
    // The Smart LP vault is StonkBrokers' audited code: exactly 23,718 bytes, its library linked.
    let code = fork.cast(&["code", SMART_LP]);
    assert_eq!((code.len() - 2) / 2, 23_718);
    assert!(code.to_lowercase().contains("4a2da8e70b51595bf564082a86336abddc62e32f"), "SmartLpLib linked");

    let RelayWorld { worker, brain, alice, signer, vault, .. } = relay_world().await?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    let me = view(&brain, &vault, "get_evm", Value::Null).await?["address"].as_str().unwrap().to_string();
    println!("relay vault's EVM address on Robinhood Chain: {me}");

    // Prices from the fork: NVDA and AMZN as Chainlink answers there, ETH at the v4 pool's mid.
    let (nvda_e8, amzn_e8, ratio) = (chainlink_e8(&fork, RHNVDA_FEED), chainlink_e8(&fork, AMZN_CHAINLINK), pool_ratio_e6(&fork));
    let nvda_usd = nvda_e8 / 100_000_000; // whole dollars for set_pair; ETH follows at the pool's ratio
    let eth_e8 = (nvda_usd as u128 * 100_000_000 * ratio / 1_000_000) as i64;
    println!("fork prices: RHNVDA ${:.2}, AMZN ${:.2}, the v4 pool's mid {:.4} NVDA per ETH (ETH ${:.2} at NVDA ${nvda_usd})",
        nvda_e8 as f64 / 1e8, amzn_e8 as f64 / 1e8, ratio as f64 / 1e6, eth_e8 as f64 / 1e8);
    let pyth = worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let mut feeds = ladder::ladder_feeds(pyth.id());
    feeds.as_array_mut().unwrap().push(json!({"name": "amzn", "contract": pyth.id(), "price_id": AMZN_FEED, "backup": common::backup(pyth.id(), AMZN_FEED)}));
    x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": feeds}})).await?.into_result()?;
    let prices = || {
        let (a, p) = (alice.clone(), pyth.clone());
        async move {
            set_pair(&a, &p, nvda_usd, ratio, 0).await?;
            a.call(p.id(), "set_price").args_json(json!({"price_id": AMZN_FEED, "price": amzn_e8.to_string(), "conf": (amzn_e8 / 2000).to_string(),
                "expo": -8, "backup": amzn_e8.to_string(), "age_s": 0})).transact().await?.into_result()?;
            anyhow::Ok(())
        }
    };

    // The owner pins each share's k from the chain (the library's was read on 2026-11-09).
    let (k_pair, k_lp) = (fork_k_pair(&fork), fork_k_smartlp(&fork, amzn_e8));
    for (name, lib, now) in [("pair token", pinned_k("pairtoken_ethnvda_zap_eth"), k_pair), ("Smart LP", pinned_k("smartlp_amzn_deposit_usdg"), k_lp)] {
        println!("{name}: k {now} on the fork, {lib} in the library ({:+.3}%)", (now as f64 / lib as f64 - 1.0) * 100.0);
    }
    for t in common::templates("robinhood") {
        let t = match t["name"].as_str().unwrap() {
            n if n.starts_with("pairtoken_") => with_k(&t, k_pair),
            n if n.starts_with("smartlp_") => with_k(&t, k_lp),
            _ => t,
        };
        x(&alice, &vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }

    // Fund it on the fork: gas money and 100 USDG (taken from the AMZN pool by impersonation).
    fork.cast(&["rpc", "anvil_setBalance", &me, "0x16345785D8A0000"]);
    fork.give(USDG, AMZN_POOL, &me, 100_000_000);
    assert_eq!(fork.balance(USDG, &me), 100_000_000);

    let base_fee = num(&fork.cast(&["base-fee"]));
    let max_fee = (base_fee * 2).max(GWEI / 100).min(GWEI);
    let mut nonce = 0u64;
    let mut call = |name: &str, values: Vec<u128>| {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce,
            "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 1_500_000u64});
        nonce += 1;
        a
    };
    let amount: u128 = 50_000_000; // 50 USDG
    let share_usd = |k: u128, pa_e8: i64, pb_e8: i64| k as f64 / 1e18 * ((pa_e8 as f64 / 1e8) * (pb_e8 as f64 / 1e8)).sqrt();

    // ---------- Smart LP (sdAMZN-FR-USDG): USDG in, shares out, and back ----------
    let r = x(&alice, &vault, "evm_call", call("approve_usdg_smartlp_amzn", vec![0, amount])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "approve executed");
    let quote = num(&fork.cast(&["call", SMART_LP, "zapDeposit(address,uint256,uint256,address)(uint256)", USDG, &amount.to_string(), "0", &me, "--from", &me]));
    // The share floor: 50 USDG ($1 each, `usd`) / (k sqrt(P_amzn)) less 1%, whatever the caller asks (0 here).
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("smartlp_amzn_deposit_usdg", vec![0, amount, 0, 0])).await?;
    let min = leaf_min(&r, 2);
    let fair = amount as f64 / 1e6 / share_usd(k_lp, amzn_e8, 100_000_000) * 1e18;
    println!("Smart LP: 50 USDG -> quote {quote} share-wei; the brain's floor {min} (fair at the feeds {fair:.0}, less 1%)");
    assert!((min as f64 / (fair * 0.99) - 1.0).abs() < 1e-6 && quote >= min, "the floor binds below the quote");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "zapDeposit executed");
    let shares = fork.balance(SMART_LP, &me);
    assert!(shares >= min);
    assert_eq!(fork.balance(USDG, &me), 50_000_000);

    // Withdraw every share with a minimum of 1: the brain signs shares x value less 1% (10 bps retention, the swap).
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("smartlp_amzn_withdraw_usdg", vec![shares, 0, 1, 0, 0])).await?;
    let min = leaf_min(&r, 2);
    let fair = shares as f64 / 1e18 * share_usd(k_lp, amzn_e8, 100_000_000) * 1e6;
    assert!((min as f64 / (fair * 0.99) - 1.0).abs() < 1e-6, "{min} vs {fair}");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "zapWithdraw executed");
    let back = fork.balance(USDG, &me) - 50_000_000;
    println!("withdrew all shares: {:.4} USDG back (floor {:.4}; {:.2}% round trip: swap fees, 10 bps retention)",
        back as f64 / 1e6, min as f64 / 1e6, (1.0 - back as f64 / amount as f64) * 100.0);
    assert_eq!(fork.balance(SMART_LP, &me), 0);
    assert!(back >= min);

    // ---------- Uniswap v4 (Universal Router) on ETHNVDA's own pool ----------
    fork.cast(&["rpc", "anvil_setBalance", &me, "0xDE0B6B3A7640000"]); // 1 ETH
    let eth = |f: &Fork| num(&f.cast(&["balance", &me]));
    let now: u128 = num(&fork.cast(&["block", "latest", "--field", "timestamp"]));
    // 17 leaves: amountIn is leaf 8, the minimum leaf 9, the deadline leaf 15, the ETH value leaf 16 (same as 8).
    let v4 = |amount: u128, min_out: u128, value_try: u128| {
        let mut v = vec![0u128; 17];
        v[8] = amount;
        v[9] = min_out;
        v[15] = now + 600;
        v[16] = value_try;
        v
    };
    let swap_in = 10_000_000_000_000_000u128; // 0.01 ETH
    let q = v4_quote(&fork, true, swap_in);
    let floor = market_floor(swap_in, eth_e8, nvda_usd * 100_000_000, 300);
    println!("v4: 0.01 ETH -> quote {:.5} NVDA; the market floor {:.5} (feeds less 3%: the pool's 1% fee and its gap to the feeds)", q as f64 / 1e18, floor as f64 / 1e18);
    // A pool 5% off the market (ETH priced 5% higher than the pool gives): the floor is over the quote, the swap reverts.
    set_pair(&alice, &pyth, nvda_usd, ratio * 105 / 100, 0).await?;
    let r = x(&alice, &vault, "evm_call", call("v4_swap_eth_nvda", v4(swap_in, 1, 0))).await?;
    let off = leaf_min_v4(&r);
    println!("  pool 5% off the market: the signed minimum {:.5} NVDA is over its quote {:.5}", off as f64 / 1e18, q as f64 / 1e18);
    assert!(off > q, "the floor is above what the pool pays");
    assert_eq!(fork.send_raw(&signed(&r)), 0, "a pool off the market: reverted, nothing bought");
    assert_eq!(fork.balance(NVDA, &me), 0);
    // At the market: the keeper's 1 wei becomes the floor; trying to attach 1 ETH instead of 0.01 signs 0.01.
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("v4_swap_eth_nvda", v4(swap_in, 1, E18))).await?;
    let req = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap().to_string();
    assert!(req.contains(&format!(r#""value":"{swap_in}""#)), "{req}");
    assert!(signed(&r).contains(&format!("{floor:064x}")), "the market floor is the signed minimum");
    let before = eth(&fork);
    assert_eq!(fork.send_raw(&signed(&r)), 1, "v4 swap ETH -> NVDA executed");
    let nvda = fork.balance(NVDA, &me);
    println!("v4 swap: 0.01 ETH -> {:.5} NVDA into the vault (ETH spent incl. gas {:.5})", nvda as f64 / 1e18, (before - eth(&fork)) as f64 / 1e18);
    assert!(nvda >= floor);

    // Back to ETH: approve Permit2, let the Universal Router pull through Permit2, swap all NVDA.
    let r = x(&alice, &vault, "evm_call", call("approve_nvda_permit2", vec![0, nvda])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1);
    let r = x(&alice, &vault, "evm_call", call("permit2_nvda_universal_router", vec![0, 0, nvda, now + 3600])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "Permit2 allowance set");
    let q = v4_quote(&fork, false, nvda);
    let floor = market_floor(nvda, nvda_usd * 100_000_000, eth_e8, 300);
    let mut v = v4(nvda, 1, 0);
    v.pop(); // no ETH value on this one: 16 leaves
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("v4_swap_nvda_eth", v)).await?;
    assert!(signed(&r).contains(&format!("{floor:064x}")), "the market floor is the signed minimum");
    let before = eth(&fork);
    assert_eq!(fork.send_raw(&signed(&r)), 1, "v4 swap NVDA -> ETH executed");
    println!("v4 swap back: {:.5} NVDA -> {:.5} ETH net of gas (quote {:.5}, floor {:.5})", nvda as f64 / 1e18, (eth(&fork) as f64 - before as f64) / 1e18, q as f64 / 1e18, floor as f64 / 1e18);
    assert_eq!(fork.balance(NVDA, &me), 0);

    // ---------- the ETHNVDA pair token: zap 0.01 ETH in, redeem all of it to ETH ----------
    let zap = 10_000_000_000_000_000u128;
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("pairtoken_ethnvda_zap_eth", vec![0, zap, 0, 0, 0])).await?;
    let min = leaf_min(&r, 2);
    let want = zap_floor(zap, eth_e8, nvda_usd * 100_000_000, k_pair, 300);
    assert!((min as f64 / want - 1.0).abs() < 1e-9, "{min} vs {want}");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "pair token zapDeposit executed");
    let pt = fork.balance(ETHNVDA, &me);
    println!("pair token: 0.01 ETH -> {:.6} ETHNVDA shares (share floor {:.6}: ${:.2} a share at the feeds, less 3%)",
        pt as f64 / 1e18, min as f64 / 1e18, share_usd(k_pair, eth_e8, nvda_usd * 100_000_000));
    assert!(pt >= min);
    let before = eth(&fork);
    prices().await?;
    let r = x(&alice, &vault, "evm_call", call("pairtoken_ethnvda_redeem_eth", vec![pt, 0, 1, 0])).await?;
    let min = leaf_min(&r, 2);
    let fair = pt as f64 * share_usd(k_pair, eth_e8, nvda_usd * 100_000_000) / (eth_e8 as f64 / 1e8);
    assert!((min as f64 / (fair * 0.97) - 1.0).abs() < 1e-9, "{min} vs {fair}");
    assert_eq!(fork.send_raw(&signed(&r)), 1, "pair token redeemZap executed");
    let got = eth(&fork) as f64 - before as f64;
    println!("redeemed all shares: {:.6} ETH back net of gas (share floor {:.6})", got / 1e18, min as f64 / 1e18);
    assert_eq!(fork.balance(ETHNVDA, &me), 0);
    Ok(())
}

/// The minimum (leaf 9) inside a v4 swap's nested calldata: the signed tx carries it as one word.
fn leaf_min_v4(r: &near_workspaces::result::ExecutionFinalResult) -> u128 {
    let log = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap().to_string();
    let v: Value = serde_json::from_str(log.trim_start_matches("EVENT_JSON:")).unwrap();
    let d = v["data"][0]["data"].as_str().unwrap().to_string();
    // The pool key's tick spacing (200 = 0xc8) is followed by hooks, zeroForOne, amountIn, then the minimum.
    let at = d.find(&format!("{:064x}", 200u128)).unwrap() + 64 * 4;
    u128::from_str_radix(&d[at + 32..at + 64], 16).unwrap()
}
