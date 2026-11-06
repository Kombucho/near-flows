//! StonkBrokers on Robinhood Chain, from a NEAR relay vault: the node library's templates
//! for Smart LP (deposit USDG into sdAMZN-FR, withdraw), Uniswap v4 through the
//! Universal Router (ETH -> NVDA -> ETH, Permit2 approvals) and the ETHNVDA pair token
//! (zap ETH in, redeem to ETH),
//! executed on a local fork of Robinhood Chain mainnet (anvil on port 9801). The
//! brain builds and checks each call, the vault signs it with its own MPC path;
//! the Smart LP shares and the USDG coming back land at the vault's own EVM
//! address, the only address a template can name.
//!
//! Needs Foundry (anvil, cast); skipped without it. ROBINHOOD_RPC overrides the public RPC.

mod common;
mod relayw;

use common::{num, signed, Fork};
use relayw::{relay_world, view, x, RelayWorld};
use serde_json::{json, Value};

const USDG: &str = "0x5fc5360d0400a0fd4f2af552add042d716f1d168";
const SMART_LP: &str = "0x5019fca4c0193908727810209b7621a90fc9f17a"; // sdAMZN-FR
const AMZN_POOL: &str = "0x8ac92da74ab5f3b1d024dc1943ad7e15dc4179ef"; // holds USDG: funds the vault on the fork
const NVDA: &str = "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec";
const ETHNVDA: &str = "0x0d142f0a847838f933114869f6f63bd618bedca8"; // StonkBrokers pair token (ETH + NVDA, Uniswap v4 position)
const V4_QUOTER: &str = "0x8dc178efb8111bb0973dd9d722ebeff267c98f94";
const ETH: &str = "0x0000000000000000000000000000000000000000";
const GWEI: u128 = 1_000_000_000;
const E18: u128 = 1_000_000_000_000_000_000;

#[tokio::test]
async fn the_vault_runs_stonkbrokers_smart_lp_uniswap_v4_and_pair_tokens_on_a_robinhood_fork() -> anyhow::Result<()> {
    let url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    let Some(fork) = Fork::start(&url, 9801).await else { return Ok(()) };
    assert_eq!(fork.cast(&["chain-id"]), "4663", "a fork of Robinhood Chain");
    println!("Robinhood Chain fork at block {}", fork.cast(&["block-number"]));

    let RelayWorld { brain, alice, keeper, signer, vault, .. } = relay_world().await?;
    x(&alice, &vault, "evm_setup", json!({"signer": signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    let me = view(&brain, &vault, "get_evm", Value::Null).await?["address"].as_str().unwrap().to_string();
    println!("relay vault's EVM address on Robinhood Chain: {me}");
    for t in common::templates("robinhood") {
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

    // 1. Approve the Smart LP vault for 50 USDG.
    let r = x(&keeper, &vault, "evm_call", call("approve_usdg_smartlp_amzn", vec![0, amount])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "approve executed");

    // 2. The Smart LP's own quote (a simulated deposit), as the keeper would ask it.
    let quote = num(&fork.cast(&["call", SMART_LP, "zapDeposit(address,uint256,uint256,address)(uint256)", USDG, &amount.to_string(), "0", &me, "--from", &me]));
    println!("Smart LP quote: 50 USDG -> {:.4} sdAMZN-FR shares", quote as f64 / 1e18);

    // 3. A keeper asking for more shares than the vault mints: StonkBrokers reverts (MinShares), nothing moves.
    let r = x(&keeper, &vault, "evm_call", call("smartlp_amzn_deposit_usdg", vec![0, amount, quote * 2, 0])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 0, "MinShares revert");
    assert_eq!(fork.balance(USDG, &me), 100_000_000);

    // 4. Deposit with minShares = 98% of the quote (their integration guide's advice).
    let r = x(&keeper, &vault, "evm_call", call("smartlp_amzn_deposit_usdg", vec![0, amount, quote * 98 / 100, 0])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "zapDeposit executed");
    let shares = fork.balance(SMART_LP, &me);
    println!("deposited 50 USDG: the vault holds {:.4} sdAMZN-FR shares", shares as f64 / 1e18);
    assert!(shares >= quote * 98 / 100);
    assert_eq!(fork.balance(USDG, &me), 50_000_000);

    // 5. Withdraw every share back to USDG; the owner's floor (shares / 1.1e12) is the least accepted.
    let r = x(&keeper, &vault, "evm_call", call("smartlp_amzn_withdraw_usdg", vec![shares, 0, 1, 0, 0])).await?;
    let raw = signed(&r);
    assert!(raw.contains(&format!("{:064x}", shares / 1_100_000_000_000)), "the owner's floor is the minimum in the signed tx");
    assert_eq!(fork.send_raw(&raw), 1, "zapWithdraw executed");
    let back = fork.balance(USDG, &me) - 50_000_000;
    println!("withdrew all shares: {:.4} USDG back ({:.2}% round trip cost: swap fees, 10 bps retention, price impact)",
        back as f64 / 1e6, (1.0 - back as f64 / amount as f64) * 100.0);
    assert_eq!(fork.balance(SMART_LP, &me), 0);
    assert!(back >= shares / 1_100_000_000_000);

    // ---------- Uniswap v4 (Universal Router) on ETHNVDA's own pool ----------
    fork.cast(&["rpc", "anvil_setBalance", &me, "0xDE0B6B3A7640000"]); // 1 ETH
    let eth = |f: &Fork| num(&f.cast(&["balance", &me]));
    let key = format!("({ETH},{NVDA},10000,200,{ETH})");
    let v4_quote = |zero_for_one: bool, amount: u128| num(&fork.cast(&["call", V4_QUOTER,
        "quoteExactInputSingle(((address,address,uint24,int24,address),bool,uint128,bytes))(uint256,uint256)",
        &format!("({key},{zero_for_one},{amount},0x)")]));
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
    let q = v4_quote(true, swap_in);
    println!("v4 quote: 0.01 ETH -> {:.4} NVDA", q as f64 / 1e18);
    // A keeper trying to attach 1 ETH instead of 0.01: the value is `same` as amountIn, so 0.01 is signed.
    let r = x(&keeper, &vault, "evm_call", call("v4_swap_eth_nvda", v4(swap_in, q * 99 / 100, E18))).await?;
    let req = r.logs().iter().find(|l| l.contains("evm_call_requested")).unwrap().to_string();
    assert!(req.contains(&format!(r#""value":"{swap_in}""#)), "{req}");
    let before = eth(&fork);
    assert_eq!(fork.send_raw(&signed(&r)), 1, "v4 swap ETH -> NVDA executed");
    let nvda = fork.balance(NVDA, &me);
    println!("v4 swap: 0.01 ETH -> {:.4} NVDA into the vault (ETH spent incl. gas {:.5})", nvda as f64 / 1e18, (before - eth(&fork)) as f64 / 1e18);
    assert!(nvda >= q * 99 / 100);

    // Back to ETH: approve Permit2, let the Universal Router pull through Permit2, swap all NVDA.
    let r = x(&keeper, &vault, "evm_call", call("approve_nvda_permit2", vec![0, nvda])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1);
    let r = x(&keeper, &vault, "evm_call", call("permit2_nvda_universal_router", vec![0, 0, nvda, (now + 3600) as u128])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "Permit2 allowance set");
    let q = v4_quote(false, nvda);
    let mut v = v4(nvda, q * 99 / 100, 0);
    v.pop(); // no ETH value on this one: 16 leaves
    let r = x(&keeper, &vault, "evm_call", call("v4_swap_nvda_eth", v)).await?;
    let before = eth(&fork);
    assert_eq!(fork.send_raw(&signed(&r)), 1, "v4 swap NVDA -> ETH executed");
    println!("v4 swap back: {:.4} NVDA -> {:.5} ETH (net of gas)", nvda as f64 / 1e18, (eth(&fork) as f64 - before as f64) / 1e18);
    assert_eq!(fork.balance(NVDA, &me), 0);

    // ---------- the ETHNVDA pair token: zap 0.01 ETH in, redeem all of it to ETH ----------
    let zap = 10_000_000_000_000_000u128;
    let r = x(&keeper, &vault, "evm_call", call("pairtoken_ethnvda_zap_eth", vec![0, zap, 0, 0, 0])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "pair token zapDeposit executed");
    let pt = fork.balance(ETHNVDA, &me);
    println!("pair token: 0.01 ETH -> {:.5} ETHNVDA shares", pt as f64 / 1e18);
    assert!(pt >= zap * 1536 / 1000, "at least the owner's floor");
    let before = eth(&fork);
    let r = x(&keeper, &vault, "evm_call", call("pairtoken_ethnvda_redeem_eth", vec![pt, 0, 0, 0])).await?;
    assert_eq!(fork.send_raw(&signed(&r)), 1, "pair token redeemZap executed");
    println!("redeemed all shares: {:.5} ETH back (net of gas)", (eth(&fork) as f64 - before as f64) / 1e18);
    assert_eq!(fork.balance(ETHNVDA, &me), 0);
    Ok(())
}
