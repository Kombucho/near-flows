//! The flow keeper runs remote nodes by itself, end to end: the REAL keeper loop
//! (scripts/keeper `flows-main.ts --send --remote`, a separate process) against a
//! sandbox brain, a Robinhood Chain fork (anvil) and a Solana fork (surfpool).
//! Nothing here steps, signs, broadcasts or reports for it.
//!
//! One vault, three flows:
//! - EVM: a Uniswap v4 buy of NVDA with 0.03 ETH. The keeper quotes the pool, takes the
//!   brain's next nonce and the owner's fee cap, broadcasts the signed swap on the fork,
//!   measures the NVDA that arrived at the vault's address and reports it.
//! - Hop then Solana: 0.1 SOL out of the vault's Intents account (the real verifier and
//!   omni-token code) to the vault's own Solana address; the keeper watches that address,
//!   reports what landed (the test plays the bridge's payout, less its fee), and the next
//!   node buys a pump.fun coin with exactly that, built with pump.fun's SDK, broadcast on
//!   the fork and reported.
//! - A template the keeper has no builder for: skipped with a log line, never guessed.
//!
//! Halfway the keeper is stopped (by its PID) while the hop is in flight, and a new one
//! started: it resumes from its state file (~/.flows/keeper, here FLOWS_KEEPER_DIR) and
//! reports, never stepping the hop again.
//!
//! Forks: anvil on 9820 (ANVIL_PORT), surfpool on 9830/9831 (SURFPOOL_PORT); skipped
//! without them. Needs make build, scripts/fetch-protocols.sh and the keeper's node modules.

mod common;
mod hopw;
mod relayw;

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use common::{num, sol_template, templates, Fork, SolFork};
use hopw::*;
use relayw::*;
use serde_json::{json, Value};

const GWEI: u128 = 1_000_000_000;
const E24: u128 = 1_000_000_000_000_000_000_000_000;
const NVDA: &str = "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec";
const PUMP_MINT: &str = "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump";
/// The bridge's SOL withdrawal fee (bridge API supported_tokens, 2026-11-06).
const SOL_FEE: u128 = 14_000;

/// The keeper, a separate process (`node --import tsx`: one PID, nothing in between); killed when dropped.
struct Keeper {
    child: Child,
    log: String,
}

impl Keeper {
    fn start(env: &[(&str, String)], log: &str) -> Keeper {
        let dir = format!("{}/../../scripts/keeper", env!("CARGO_MANIFEST_DIR"));
        let out = std::fs::OpenOptions::new().create(true).append(true).open(log).unwrap();
        let child = Command::new("node")
            .args(["--import", "tsx", "src/flows-main.ts", "--send", "--remote", "--tick", "2"])
            .current_dir(dir)
            .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
            .stdout(Stdio::from(out.try_clone().unwrap()))
            .stderr(Stdio::from(out))
            .spawn()
            .expect("node");
        Keeper { child, log: log.to_string() }
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

impl Drop for Keeper {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn vault_state(w: &RelayWorld) -> anyhow::Result<Value> {
    Ok(w.brain.view("get_vault").args_json(json!({"vault": w.vault})).await?.json()?)
}

fn node(v: &Value, flow: u32, i: usize) -> Value {
    v["flows"][flow as usize]["ledger"]["nodes"][i].clone()
}

fn u(v: &Value) -> u128 {
    v.as_str().unwrap_or("0").parse().unwrap()
}

/// Wait (up to `secs`) until `ok` holds for the vault's record; the keeper's log on timeout.
async fn until(w: &RelayWorld, k: &Keeper, secs: u64, what: &str, ok: impl Fn(&Value) -> bool) -> anyhow::Result<Value> {
    let t = Instant::now();
    loop {
        let v = vault_state(w).await?;
        if ok(&v) {
            return Ok(v);
        }
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}\n--- keeper log ---\n{}", k.log());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[tokio::test]
async fn the_keeper_runs_an_evm_swap_a_hop_and_a_pump_fun_buy_by_itself_and_resumes_after_a_restart() -> anyhow::Result<()> {
    let url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    let Some(evm) = Fork::start(&url, 9820).await else { return Ok(()) };
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public RPC only
    let Some(sol) = SolFork::start(9830).await else { return Ok(()) };
    assert_eq!(evm.cast(&["chain-id"]), "4663");
    let w = relay_world().await?;
    let i = intents(&w).await?;

    // The owner's setup: Robinhood Chain with the node library's templates, Solana with a pump.fun buy.
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    for t in templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    let v = vault_state(&w).await?;
    let (evm_me, sol_me) = (v["evm"]["address"].as_str().unwrap().to_string(), v["sol"]["address"].as_str().unwrap().to_string());
    evm.cast(&["rpc", "anvil_setBalance", &evm_me, "0xDE0B6B3A7640000"]); // 1 ETH, for the swap and its gas
    sol.rpc_call("surfnet_setAccount", json!([sol_me, {"lamports": 50_000_000u64}])); // 0.05 SOL, for fees and the coin's account
    let probe = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, "50000000", "5"]);
    let per_lamport = probe["tokens"].as_str().unwrap_or_else(|| panic!("pump.fun helper: {probe}")).parse::<u128>()? / 52_500_000;
    x(&w.alice, &w.vault, "sol_template", json!({"template": sol_template("pump_buy", &[("mint", PUMP_MINT), ("max_lamports", "100000000"), ("floor_per_lamport", &(per_lamport / 2).to_string())])})).await?.into_result()?;
    i.bridge_in(&i.sol, &w.vault, SOL).await?;
    x(&w.alice, &w.vault, "intents_hop", hop_rule(i.sol.id(), "sol", SOL / 2, SOL / 10_000)).await?.into_result()?;

    // Flow 0, EVM: buy NVDA with 0.03 ETH on Uniswap v4.
    let remote = |input: &str, label: &str, call: Value, out: Value, cap: Option<u128>, next: Value| json!({"input": input, "when": {"when": "now"}, "next": next,
        "action": {"label": label, "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": call, "out": out, "max_out_per_in": cap.map(|c| c.to_string())}}});
    let buy = remote("evm:4663:eth", "Buy NVDA", json!({"chain": "evm", "template": "v4_swap_eth_nvda", "amount_leaf": 8, "min_leaf": 9, "deadline_leaf": 15}),
        json!(format!("evm:4663:{NVDA}")), Some(20 * E24), json!([]));
    let f_evm: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "NVDA", "nodes": [buy]})).await?.json()?;
    let eth_in = 30_000_000_000_000_000u128;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f_evm, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    // Flow 1: hop 0.1 SOL out of Intents to the vault's Solana address, then buy the coin with exactly what landed.
    let pump = remote("sol:SOL", "Buy the coin", json!({"chain": "sol", "template": "pump_buy", "amount_field": 1, "min_field": 0}),
        json!(format!("sol:{PUMP_MINT}")), Some(per_lamport * 4 * E24), json!([]));
    let f_sol: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "Intents -> Solana -> coin", "nodes": [
        hop_node(&format!("intents:{}", i.sol.id()), "sol:SOL", json!([{"to": 1, "bps": 10_000}])), pump]})).await?.json()?;
    let hop = SOL / 10;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f_sol, "node": 0, "amount": hop.to_string()})).await?.into_result()?;
    // Flow 2: a template the keeper has no builder for.
    let manual = remote("evm:4663:eth", "Approve by hand", json!({"chain": "evm", "template": "approve_nvda_permit2", "amount_leaf": 1}), Value::Null, None, json!([]));
    let f_manual: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "manual", "nodes": [manual]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f_manual, "node": 0, "amount": "1"})).await?.into_result()?;

    // The keeper: its own key, the sandbox's RPC, the forks' RPCs, a state folder of its own.
    let target = format!("{}/../target/keeper-remote-fork-{}", env!("CARGO_MANIFEST_DIR"), std::process::id());
    std::fs::create_dir_all(&target)?;
    let factory = w.vault.as_str().split_once('.').unwrap().1.to_string();
    let env = vec![
        ("NEAR_NETWORK", "sandbox".to_string()),
        ("NEAR_RPC", w.worker.rpc_addr()),
        ("FACTORY_ID", factory),
        ("KEEPER_ACCOUNT_ID", w.keeper.id().to_string()),
        ("KEEPER_PRIVATE_KEY", w.keeper.secret_key().to_string()),
        ("EVM_RPC_4663", evm.rpc.clone()),
        ("SOLANA_RPC", sol.rpc.clone()),
        ("FLOWS_KEEPER_DIR", target.clone()),
        ("KEEPER_DEBUG", "1".to_string()),
    ];
    let log = format!("{target}/keeper.log");
    let mut k = Keeper::start(&env, &log);

    // 1. The EVM swap, stepped, broadcast and reported by the keeper alone.
    let v = until(&w, &k, 180, "the NVDA buy reported", |v| node(v, f_evm, 0)["runs"] == 1).await?;
    let nvda = evm.balance(NVDA, &evm_me);
    assert!(nvda > 0, "NVDA arrived at the vault's address");
    assert_eq!(u(&node(&v, f_evm, 0)["out_total"]), nvda, "the keeper reported exactly what arrived");
    assert_eq!(u(&node(&v, f_evm, 0)["inbox"]), 0);
    assert_eq!(v["evm"]["next_nonce"], 1, "the brain's nonce, used once");
    assert_eq!(num(&evm.cast(&["nonce", &evm_me])), 1, "and landed at it");
    println!("keeper: 0.03 ETH -> {:.6} NVDA on the Robinhood fork, reported", nvda as f64 / 1e18);

    // 2. The hop: sent by NEAR, waiting for the bridge. Stop the keeper while it waits.
    until(&w, &k, 120, "the hop sent", |v| v["remote"].as_array().is_some_and(|r| r.iter().any(|p| p["flow"] == f_sol && p["node"] == 0 && p["signed"] == true))).await?;
    assert_eq!(i.mt(&w.vault, i.sol.id()).await?, SOL - hop, "0.1 SOL left the vault's Intents account");
    let state = format!("{target}/{}.json", w.keeper.id());
    // Stopped once the keeper holds only the hop (the swap's report is back and forgotten), so the
    // restart below resumes exactly one step. (Stopped mid-report, the next keeper finds that step
    // no longer pending and forgets it: also fine, but not what this checks.)
    let only_hop = |s: &str| serde_json::from_str::<Value>(s).ok().and_then(|v| v["inflight"].as_array().cloned())
        .is_some_and(|a| a.len() == 1 && a[0]["kind"] == "hop" && a[0]["status"] == "sent");
    let t = Instant::now();
    while !only_hop(&std::fs::read_to_string(&state).unwrap_or_default()) {
        assert!(t.elapsed() < Duration::from_secs(60), "the state file never held just the hop: {}", k.log());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    k.stop();
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&state)?)?;
    let f = &kept["inflight"].as_array().unwrap().iter().find(|f| f["kind"] == "hop").unwrap().clone();
    assert_eq!((f["status"].as_str(), f["address"].as_str()), (Some("sent"), Some(sol_me.as_str())), "{kept}");
    println!("keeper stopped with the hop in flight: {}", f["nearTx"]);

    // A new keeper: it resumes the hop from the state file and doesn't step it again.
    let mut k = Keeper::start(&env, &log);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let v = vault_state(&w).await?;
    assert_eq!(node(&v, f_sol, 0)["runs"], 0, "not reported: nothing landed yet");
    assert_eq!(i.mt(&w.vault, i.sol.id()).await?, SOL - hop, "not stepped again");
    assert!(k.log().contains("resuming 1 remote step(s) in flight"), "{}", k.log());

    // The bridge pays the vault's own Solana address, less its fee (played here).
    let landed = hop - SOL_FEE;
    let lamports = sol.rpc_call("getBalance", json!([sol_me]))["result"]["value"].as_u64().unwrap() as u128;
    sol.rpc_call("surfnet_setAccount", json!([sol_me, {"lamports": (lamports + landed) as u64}]));
    let v = until(&w, &k, 120, "the hop reported", |v| node(v, f_sol, 0)["runs"] == 1).await?;
    assert_eq!(u(&node(&v, f_sol, 0)["out_total"]), landed, "what landed, measured at the vault's address");

    // 3. The pump.fun buy with exactly what landed: built, stepped, broadcast and reported by the keeper.
    let v = until(&w, &k, 180, "the coin bought and reported", |v| node(v, f_sol, 1)["runs"] == 1).await?;
    let coins = u(&sol.helper(&["balance", &sol_me, PUMP_MINT])["amount"]);
    assert!(coins > 0);
    assert_eq!(u(&node(&v, f_sol, 1)["out_total"]), coins, "the keeper reported exactly the coins that arrived");
    assert_eq!((u(&node(&v, f_sol, 0)["inbox"]), u(&node(&v, f_sol, 1)["inbox"])), (0, 0));
    assert_eq!(v["remote"], json!([]), "nothing pending on the brain");
    assert_eq!(node(&v, f_manual, 0)["runs"], 0);
    let log = k.log();
    for l in log.lines().filter(|l| !l.contains("Approve by hand") && !l.contains("name this keeper")) {
        println!("KEEPER| {l}");
    }
    k.stop();
    assert!(log.contains("no builder for approve_nvda_permit2: run it by hand"), "{log}");
    assert!(!log.contains("owner must settle"), "every step was this keeper's and landed: nothing for the owner\n{log}");
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(&state)?)?;
    assert_eq!(kept["inflight"], json!([]), "the state file is empty once everything is reported");
    println!("keeper: hop of {hop} lamports -> {landed} landed at {sol_me} -> {coins} coins on the Solana fork, all reported");
    let _ = std::fs::remove_dir_all(&target);
    Ok(())
}
