//! Hylo (Solana) through a relay vault, executed for real on a local fork of Solana mainnet
//! (surfpool, public RPC): the vault mints xSOL (Hylo's leveraged SOL) from jitoSOL and redeems
//! it back, redeems hyUSD (Hylo's stablecoin) to jitoSOL, and moves hyUSD in and out of Hylo's
//! Earn (stability) pool. The keeper builds every transaction with Hylo's own Rust SDK
//! (scripts/hylo: hylo-quotes / hylo-clients 2.7.2, the router's route_v2 exactly as the SDK
//! builds it), the brain checks every byte against the templates published in
//! nodes/sol-templates.json (hylo_*), the relay vault signs (test signer: a real Ed25519 key
//! standing in for the MPC network), and the fork executes it against mainnet's state.
//!
//! The fork keeps the first copy of every account it reads, and Hylo refuses a Pyth price older
//! than 10 s (2.5 s for hyUSD). So before quoting and again right before sending, `hylo-fork
//! fresh` copies today's prices from mainnet again and stamps them as published at the fork's
//! own time (the prices themselves are mainnet's, untouched).
//!
//! Needs surfpool, the keeper's node modules (scripts/keeper: npm install --ignore-scripts) and
//! cargo (scripts/hylo is built on first use). Port 9720/9721.

mod common;
mod relayw;

use common::{sol_sig, sol_template, SolFork};
use near_workspaces::result::ExecutionFinalResult;
use relayw::*;
use serde_json::{json, Value};
use std::process::Command;

const JITOSOL: &str = "J1toso1uCk3RLmjorhTtrVwY9HJ7X8V9yYac6Y7kGCPn";
const XSOL: &str = "4sWNB8zGWHkh6UnmwiEtzNxL4XrN7uK9tosbESbJFfVs";
const HYUSD: &str = "5YMkXAYccHSGnHn9nob9xEvv6Pvka9DZWH7nTbotTu9E";
const SHYUSD: &str = "HnnGv3HrSqjRpgdFmx7vQGjntNEoex1SU4e9Lxcxuihz";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// Someone else's wallet (a well-known mainnet address): proceeds must never go to its accounts.
const STRANGER: &str = "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin";

/// Hylo's SDK helper (scripts/hylo) against the fork: its last line, as JSON. `Err` with its
/// message when the SDK refuses (e.g. an operation Hylo has switched off).
fn hylo(fork: &SolFork, args: &[&str]) -> Result<Value, String> {
    let manifest = format!("{}/../../scripts/hylo/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    let out = Command::new("cargo").args(["run", "--release", "-q", "--manifest-path", &manifest, "--"]).args(args).env("SOLANA_RPC", &fork.rpc).output().expect("cargo");
    let text = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str(text.trim().lines().last().unwrap_or("")) {
        Ok(v) if out.status.success() => Ok(v),
        _ => Err(String::from_utf8_lossy(&out.stderr).trim().to_string()),
    }
}

fn units(v: &Value, k: &str) -> u128 {
    v[k].as_str().unwrap_or_else(|| panic!("{k} in {v}")).parse().unwrap()
}

#[tokio::test]
async fn a_relay_vault_mints_and_redeems_hylo_xsol_and_hyusd_on_a_solana_fork() -> anyhow::Result<()> {
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public RPC only
    let Some(fork) = SolFork::start(9720).await else { return Ok(()) };
    let fresh = || hylo(&fork, &["fresh"]).unwrap_or_else(|e| panic!("fresh prices: {e}"));
    let info = hylo(&fork, &["info"]).unwrap_or_else(|e| panic!("hylo-fork: {e}"));
    println!("Hylo: router {}, exchange {}, earn pool {}", info["router"], info["exchange"], info["earn_pool"]);
    assert_eq!((info["xsol"].as_str(), info["hyusd"].as_str(), info["shyusd"].as_str()), (Some(XSOL), Some(HYUSD), Some(SHYUSD)));

    let w = relay_world().await?;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let me = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    println!("relay vault's Solana address: {me}");
    // On the fork: 1 SOL for fees and rent, 2 jitoSOL and 100 hyUSD in the vault's own token accounts.
    fork.rpc_call("surfnet_setAccount", json!([me, {"lamports": 1_000_000_000u64}]));
    fork.rpc_call("surfnet_setTokenAccount", json!([me, JITOSOL, {"amount": 2_000_000_000u64}, TOKEN]));
    fork.rpc_call("surfnet_setTokenAccount", json!([me, HYUSD, {"amount": 100_000_000u64}, TOKEN]));
    let held = |mint: &str| units(&fork.helper(&["balance", &me, mint]), "amount");

    // The owner approves each Hylo action at today's quote: at most so much in, the expected out
    // at least half of today's rate (a broken quote can't pass), slippage at most 1%.
    let s = fresh();
    println!("SOL/USD {} on mainnet just now; Hylo's oracle window {} s", s["sol_usd"], s["oracle_interval_secs"]);
    let rate = |a: &str, b: &str, amount: &str| {
        let q = hylo(&fork, &["route", &me, a, b, amount, "50"]).unwrap_or_else(|e| panic!("quote {a} -> {b}: {e}"));
        println!("quote: {amount} {a} units -> {} {b} units (fee {} e{} of {}): {}", q["out"], q["fee"], q["fee_exp"], q["fee_mint"], q["op"]);
        units(&q, "out")
    };
    let approve = |name: &str, max_in: &str, floor_num: u128, floor_den: &str| {
        sol_template(name, &[("max_in", max_in), ("floor_num", &floor_num.to_string()), ("floor_den", floor_den), ("max_slippage_bps", "100")])
    };
    let xsol_per_jitosol = rate("jitosol", "xsol", "1000000000");
    let templates = vec![
        approve("hylo_mint_xsol", "1500000000", xsol_per_jitosol / 2, "1000000000"),
        approve("hylo_redeem_hyusd", "100000000", rate("hyusd", "jitosol", "50000000") / 2, "50000000"),
        approve("hylo_earn_deposit", "100000000", rate("hyusd", "shyusd", "20000000") / 2, "20000000"),
        // hyUSD can't be quoted while Hylo has minting off; the floor is half of SOL/USD per jitoSOL
        // (a jitoSOL is worth at least a SOL).
        approve("hylo_mint_hyusd", "1000000000", (s["sol_usd"].as_f64().unwrap() * 1e6 / 2.0) as u128, "1000000000"),
    ];
    for t in templates {
        x(&w.alice, &w.vault, "sol_template", json!({"template": t})).await?.into_result()?;
    }

    let sign = |msg: &Value| {
        let (k, v, m) = (w.alice.clone(), w.vault.clone(), msg["message"].as_str().unwrap().to_string());
        async move { x(&k, &v, "sol_sign_tx", json!({"message": m})).await }
    };
    let refused = |r: &ExecutionFinalResult, why: &str| {
        assert!(sol_sig(r).is_none(), "signed, expected a refusal ({why})");
        assert!(format!("{:?}", r.failures()).contains(why), "expected '{why}': {:?}", r.failures());
    };
    // Signed by the vault, then sent to the fork with the prices stamped fresh again: the vault's
    // signing round trip takes longer than Hylo's oracle window.
    let send = |msg: &Value, r: &ExecutionFinalResult| {
        let sig = sol_sig(r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
        fresh();
        fork.helper(&["send", msg["message"].as_str().unwrap(), &sig])
    };
    let landed = |sent: &Value| assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");

    // 1. Mint xSOL with 1 jitoSOL.
    fresh();
    let m = hylo(&fork, &["route", &me, "jitosol", "xsol", "1000000000", "50"]).unwrap();
    let r = sign(&m).await?;
    println!("GAS relay sol_sign_tx (Hylo mint xSOL): {:.1} Tgas", tgas(&r));
    let sent = send(&m, &r);
    landed(&sent);
    let xsol = held(XSOL);
    println!("minted on the fork: 1 jitoSOL -> {:.4} xSOL (quoted {:.4}, Hylo's 1% fee taken in jitoSOL), jitoSOL left {:.4} (tx {})",
        xsol as f64 / 1e6, units(&m, "out") as f64 / 1e6, held(JITOSOL) as f64 / 1e9, sent["signature"]);
    assert!(xsol >= units(&m, "out") * 995 / 1000, "at least the quote less 0.5% slippage");
    assert_eq!(held(JITOSOL), 1_000_000_000);

    // 2. Refused, nothing signed: over the 1.5 jitoSOL cap; an expected-out under the owner's
    //    floor (a keeper asking Hylo for almost nothing); the same mint with the xSOL paid to
    //    someone else's token account; a mint for someone else's wallet.
    fresh();
    refused(&sign(&hylo(&fork, &["route", &me, "jitosol", "xsol", "1600000000", "50"]).unwrap()).await?, "over the cap");
    refused(&sign(&hylo(&fork, &["route", &me, "jitosol", "xsol", "500000000", "50", "1"]).unwrap()).await?, "under the floor");
    let mine = fork.helper(&["ata", &me, XSOL])["ata"].as_str().unwrap().to_string();
    let theirs = fork.helper(&["ata", STRANGER, XSOL])["ata"].as_str().unwrap().to_string();
    let base = hylo(&fork, &["route", &me, "jitosol", "xsol", "500000000", "50"]).unwrap();
    let diverted = fork.helper(&["swap-key", base["message"].as_str().unwrap(), &mine, &theirs]);
    let r = sign(&diverted).await?;
    assert!(sol_sig(&r).is_none(), "xSOL paid to another wallet's account was signed");
    // (The SDK's create-idempotent for the output account is the first thing named: refused there.)
    let why = format!("{:?}", r.failures());
    println!("refused, xSOL to {theirs}: {}", why.split("refused: ").nth(1).and_then(|s| s.split("\\\")").next()).unwrap_or(&why));
    assert!(sol_sig(&sign(&hylo(&fork, &["route", STRANGER, "jitosol", "xsol", "500000000", "50"]).unwrap()).await?).is_none(), "another wallet's mint was signed");

    // 3. Hylo itself holds the line too: an expected-out over what Hylo pays (twice the quote) is
    //    within the owner's template, so the vault signs, and Hylo's program reverts it on chain.
    let greedy = hylo(&fork, &["route", &me, "jitosol", "xsol", "500000000", "50", &(units(&base, "out") * 2).to_string()]).unwrap();
    let r = sign(&greedy).await?;
    let sent = send(&greedy, &r);
    assert!(sent["signature"].is_null() || !sent["err"].is_null(), "Hylo paid twice its quote? {sent}");
    println!("Hylo reverted the over-asked mint on chain: {}", sent["error"].as_str().unwrap_or("").chars().take(160).collect::<String>());
    assert_eq!(held(JITOSOL), 1_000_000_000, "nothing moved");

    // 4. Redeem all the xSOL back to jitoSOL (the owner approves the redeem at today's rate).
    fresh();
    let all = xsol.to_string();
    let back = rate("xsol", "jitosol", &all);
    x(&w.alice, &w.vault, "sol_template", json!({"template": approve("hylo_redeem_xsol", &all, back / 2, &all)})).await?.into_result()?;
    fresh();
    let rx = hylo(&fork, &["route", &me, "xsol", "jitosol", &all, "50"]).unwrap();
    let r = sign(&rx).await?;
    let sent = send(&rx, &r);
    landed(&sent);
    let jito = held(JITOSOL);
    println!("redeemed on the fork: {:.4} xSOL -> +{:.6} jitoSOL (quoted {:.6}); round trip cost {:.2}% (Hylo's mint + redeem fees and the price move) (tx {})",
        xsol as f64 / 1e6, (jito - 1_000_000_000) as f64 / 1e9, units(&rx, "out") as f64 / 1e9, 100.0 * (1.0 - (jito - 1_000_000_000) as f64 / 1e9), sent["signature"]);
    assert_eq!(held(XSOL), 0);
    assert!(jito - 1_000_000_000 >= units(&rx, "out") * 995 / 1000);

    // 5. hyUSD: minting it is Hylo's call. When Hylo has it switched off (its collateral ratio
    //    below the mint threshold), the SDK won't quote it and the program refuses it on chain
    //    even though the vault's template allows it; otherwise it mints.
    fresh();
    match hylo(&fork, &["route", &me, "jitosol", "hyusd", "100000000", "50"]) {
        Ok(q) => {
            let before = held(HYUSD);
            let r = sign(&q).await?;
            landed(&send(&q, &r));
            println!("minted on the fork: 0.1 jitoSOL -> {:.4} hyUSD (quoted {:.4})", (held(HYUSD) - before) as f64 / 1e6, units(&q, "out") as f64 / 1e6);
        }
        Err(e) => {
            println!("Hylo's SDK won't quote a hyUSD mint now: {}", e.lines().last().unwrap_or(""));
            assert!(e.contains("disabled"), "{e}");
            let expect = (s["sol_usd"].as_f64().unwrap() * 1e5) as u128; // 0.1 jitoSOL at SOL/USD
            let q = hylo(&fork, &["build", &me, "jitosol", "hyusd", "100000000", &expect.to_string(), "50"]).unwrap();
            let r = sign(&q).await?;
            let sent = send(&q, &r);
            assert!(sent["signature"].is_null() || !sent["err"].is_null(), "a switched-off mint went through: {sent}");
            println!("the vault signed (template ok), Hylo refused it on chain: {}", sent["error"].as_str().unwrap_or("").chars().take(160).collect::<String>());
        }
    }

    // 6. Redeem 50 hyUSD to jitoSOL.
    fresh();
    let jito = held(JITOSOL);
    let rh = hylo(&fork, &["route", &me, "hyusd", "jitosol", "50000000", "50"]).unwrap();
    let r = sign(&rh).await?;
    landed(&send(&rh, &r));
    let got = held(JITOSOL) - jito;
    println!("redeemed on the fork: 50 hyUSD -> {:.6} jitoSOL (quoted {:.6})", got as f64 / 1e9, units(&rh, "out") as f64 / 1e9);
    assert!(got >= units(&rh, "out") * 995 / 1000);
    assert_eq!(held(HYUSD), 50_000_000);

    // 7. The Earn pool: deposit 20 hyUSD for sHYUSD, then (owner-approved at the pool's rate)
    //    withdraw it all back to hyUSD, if Hylo lets it out yet.
    fresh();
    let d = hylo(&fork, &["route", &me, "hyusd", "shyusd", "20000000", "50"]).unwrap();
    let r = sign(&d).await?;
    landed(&send(&d, &r));
    let shares = held(SHYUSD);
    println!("deposited on the fork: 20 hyUSD -> {:.4} sHYUSD (quoted {:.4})", shares as f64 / 1e6, units(&d, "out") as f64 / 1e6);
    assert!(shares >= units(&d, "out") * 995 / 1000);
    fresh();
    match hylo(&fork, &["route", &me, "shyusd", "hyusd", &shares.to_string(), "50"]) {
        Ok(q) => {
            let all = shares.to_string();
            x(&w.alice, &w.vault, "sol_template", json!({"template": approve("hylo_earn_withdraw", &all, units(&q, "out") / 2, &all)})).await?.into_result()?;
            fresh();
            let q = hylo(&fork, &["route", &me, "shyusd", "hyusd", &all, "50"]).unwrap();
            let before = held(HYUSD);
            let r = sign(&q).await?;
            let sent = send(&q, &r);
            if sent["err"].is_null() && sent["signature"].is_string() {
                println!("withdrew on the fork: {:.4} sHYUSD -> {:.4} hyUSD (quoted {:.4})", shares as f64 / 1e6, (held(HYUSD) - before) as f64 / 1e6, units(&q, "out") as f64 / 1e6);
            } else {
                println!("signed, but Hylo keeps the deposit in the pool for now: {}", sent["error"].as_str().unwrap_or("").chars().take(160).collect::<String>());
            }
        }
        Err(e) => println!("Hylo's SDK won't quote the withdrawal yet: {}", e.lines().last().unwrap_or("")),
    }
    Ok(())
}
