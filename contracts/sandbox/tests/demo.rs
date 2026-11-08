//! The hackathon demo, one run: ONE relay vault (Alice's 10 KB body, all rules
//! in the shared brain) trades on three chains, every step decided and signed
//! by the vault, executed on local forks of mainnet.
//!
//! 1. NEAR: wrap NEAR and buy a memecoin on the real Rhea DEX code, guarded by
//!    the owner's fixed floor (a memecoin has no trusted price, so no market
//!    floor: the vault says so plainly); a pumped pool is refused.
//! 2. Solana: buy a pump.fun coin through a flow node holding the SOL it may spend (surfpool fork).
//! 3. Solana: open a Meteora DLMM position (the owner co-signs), let a trader
//!    pay it fees, and run a flow that sweeps the fees into the pump.fun coin:
//!    claim node -> keeper-reported (capped) output -> buy node.
//! 4. Robinhood Chain: buy NVDA on Uniswap v4, sell a third at each oracle rung,
//!    route every sale into StonkBrokers' ETHNVDA pair token (anvil fork).
//!
//! Narrative lines start with "DEMO|" (scripts/demo/run.sh shows only those).
//! Forks: anvil on 9500 (Robinhood Chain), surfpool on 9510/9511 (Solana, public RPC).

mod common;
mod ladder;
mod relayw;
mod rhea;

use common::{num, sol_sig, sol_template, signed, wasm, Fork, SolFork};
use ladder::{event, inbox, send, v4_quote, v4_values, E18, E24, ETHNVDA, GWEI, NVDA, NVDA_FEED};
use near_workspaces::types::NearToken;
use near_workspaces::Contract;
use relayw::*;
use rhea::NEAR;
use serde_json::{json, Value};

const NEAR_ID: &str = "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750";
const PUMP_MINT: &str = "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump";
const DLMM_POOL: &str = "5rCf1DM8LjKTw4YqhnoLcngyZYeNnQqztScTogYHAS6";
const USDC: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
const WSOL: &str = "So11111111111111111111111111111111111111112";
const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
/// MEME has 6 decimals; at NEAR $2 and MEME $5.1204e-8, one NEAR is worth 39,059,448.48 MEME.
const MEME_UNITS_PER_NEAR: u128 = 39_059_448_480_587;

macro_rules! say {
    () => { println!("DEMO|") };
    ($($t:tt)*) => { println!("DEMO| {}", format!($($t)*)) };
}

fn header(title: &str) {
    say!();
    say!("━━━ {title} ━━━");
}

/// `v` in units of 10^-`dec`, with thousands separators and `shown` decimals.
fn amt(v: u128, dec: u32, shown: usize) -> String {
    let whole = v / 10u128.pow(dec);
    let frac = if shown == 0 { String::new() } else { format!(".{:0w$}", v % 10u128.pow(dec), w = dec as usize)[..shown + 1].to_string() };
    let s = whole.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out + &frac
}

fn u(v: &Value) -> u128 {
    v.as_str().unwrap_or_else(|| panic!("not a number: {v}")).parse().unwrap()
}

async fn ledger(w: &RelayWorld, f: u32) -> anyhow::Result<Value> {
    Ok(w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": f})).await?.json::<Value>()?["ledger"].clone())
}

/// The flow's ledger as the canvas would show it: what waits at each node (its input token),
/// what each node has produced so far (its output token). Per node: label, (decimals, unit) in, out.
async fn show_ledger(w: &RelayWorld, f: u32, nodes: &[(&str, (u32, &str), (u32, &str))]) -> anyhow::Result<()> {
    let l = ledger(w, f).await?;
    for (i, (label, (di, ui), (dout, uout))) in nodes.iter().enumerate() {
        let n = &l["nodes"][i];
        say!("   │ node {i} {label:<16} waiting {:>14} {ui:<6} produced {:>18} {uout}", amt(u(&n["inbox"]), *di, 6), amt(u(&n["out_total"]), *dout, 6));
    }
    Ok(())
}

async fn set_pyth(w: &RelayWorld, pyth: &Contract, id: &str, usd_e8: i64, age_s: i64) -> anyhow::Result<()> {
    w.alice.call(pyth.id(), "set_price").args_json(json!({"price_id": id, "price": usd_e8.to_string(), "conf": (usd_e8 / 2000).to_string(),
        "expo": -8, "backup": usd_e8.to_string(), "age_s": age_s})).transact().await?.into_result()?;
    Ok(())
}

#[tokio::test]
async fn one_relay_vault_buys_memecoins_on_near_and_solana_sweeps_lp_fees_and_ladders_nvda() -> anyhow::Result<()> {
    let started = std::time::Instant::now();
    std::env::remove_var("SURFPOOL_DATASOURCE_RPC_URL"); // public Solana RPC only
    let rh_url = std::env::var("ROBINHOOD_RPC").unwrap_or_else(|_| "https://rpc.mainnet.chain.robinhood.com".into());
    // The forks and the NEAR node start together: the forks take the longest.
    let (rh, sol, w) = tokio::join!(Fork::start(&rh_url, 9500), SolFork::start(9510), relay_world());
    let (Some(rh), Some(sol)) = (rh, sol) else { panic!("the demo needs anvil (Foundry) and surfpool") };
    let w = w?;
    assert_eq!(rh.cast(&["chain-id"]), "4663", "a fork of Robinhood Chain");

    // ---------- Alice's vault, set up once for every chain ----------
    let pyth = w.worker.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    x(&w.alice, &w.vault, "sol_setup", json!({"signer": w.signer.id(), "max_cu_price": 1_000_000u64, "max_cu_limit": 1_400_000u32})).await?.into_result()?;
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64,
        "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64})).await?.into_result()?;
    // Prices: NEAR from Pyth (mock_pyth, Pyth's interface) confirmed by a second publisher;
    // NVDA single-source (said plainly: it fires the ladder's rungs, no market floor reads it). At most 60 s old.
    // The memecoin has no feed at all: no trusted price exists for it.
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 500, "feeds": [
        {"name": "near", "contract": pyth.id(), "price_id": NEAR_ID, "backup": common::backup(pyth.id(), NEAR_ID)},
        {"name": "nvda", "contract": pyth.id(), "price_id": NVDA_FEED, "backup": common::single_source()}]}})).await?.into_result()?;
    let sol_me = w.brain.view("get_sol").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let evm_me = w.brain.view("get_evm").args_json(json!({"vault": w.vault})).await?.json::<Value>()?["address"].as_str().unwrap().to_string();
    let relay_kb = wasm("relay").len() as f64 / 1024.0;
    say!("NEAR Flows demo: one vault, three chains, every step decided by the vault");
    say!("Alice's vault {} : a {relay_kb:.1} KB relay, keyless; its rules live in the shared brain {}", w.vault, w.brain.id());
    say!("  its Solana address {sol_me}");
    say!("  its EVM address    {evm_me}");
    say!("  signatures: a test signer (real secp256k1 / Ed25519 keys) standing in for NEAR's MPC network");

    // =====================================================================
    header("1. Buy a memecoin on Rhea (NEAR), guarded by the owner's fixed floor");
    let lp = w.worker.root_account()?;
    let dex = rhea::deploy(&w.worker).await?;
    let meme = w.worker.dev_deploy(&wasm("mock_ft")).await?;
    meme.call("new").args_json(json!({"owner_id": lp.id(), "total_supply": (u128::MAX / 4).to_string(), "name": "MEME", "symbol": "MEME", "decimals": 6, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    let pool = dex.pool(&lp, meme.id(), 10_000 * NEAR, 10_000 * MEME_UNITS_PER_NEAR).await?;
    say!("Rhea (the real testnet contract) has a wNEAR/MEME pool: 10,000 wNEAR : {} MEME", amt(10_000 * MEME_UNITS_PER_NEAR, 6, 0));
    for t in [dex.wrap.id(), meme.id()] {
        rhea::register(&lp, t, &w.vault).await?;
    }
    say!("MEME is a memecoin: no trusted price exists, so no market floor. Alice sets a fixed floor and a small size instead");
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": dex.wrap.id(), "methods": ["near_deposit"]}, {"contract": meme.id()}, {"contract": dex.dex.id()}], "remove": []}))
        .await?.into_result()?;
    let mut buy = dex.swap_recipe(pool, dex.wrap.id(), meme.id(), None);
    buy["label"] = json!("Buy MEME on Rhea");
    buy["price"] = Value::Null;
    // The owner's fixed floor, not the pool's word: 1% under today's price (MEME units per wNEAR; scaled
    // 1e24 per input unit, and a wNEAR is 1e24 units, so the two cancel).
    let per_in = MEME_UNITS_PER_NEAR * 99 / 100;
    buy["min_out_per_in"] = json!(per_in.to_string());
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "Ape into MEME", "nodes": [
        {"input": "near", "action": dex.wrap_recipe(), "when": {"when": "now"}, "next": [{"to": 1, "bps": 10_000}]},
        {"input": dex.wrap.id(), "action": buy, "when": {"when": "now"}}]})).await?.json()?;
    w.alice.transfer_near(&w.vault, NearToken::from_near(12)).await?.into_result()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * NEAR).to_string()})).await?.into_result()?;
    say!("flow \"Ape into MEME\": [wrap NEAR] -> [buy MEME on Rhea, fixed floor {} MEME per wNEAR]; Alice funds it with 10 NEAR", amt(per_in, 6, 0));
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    say!("keeper steps node 0: 10 NEAR wrapped into {} wNEAR, routed to node 1", amt(rhea::balance(&dex.wrap, &w.vault).await?, 24, 2));

    // A whale pumps the pool just before the vault's buy.
    let whale_in = 300 * NEAR;
    lp.call(dex.wrap.id(), "near_deposit").deposit(NearToken::from_yoctonear(whale_in)).transact().await?.into_result()?;
    let whale_before = rhea::balance(&meme, lp.id()).await?;
    dex.trade(&lp, pool, dex.wrap.id(), meme.id(), whale_in).await?;
    let whale_got = rhea::balance(&meme, lp.id()).await? - whale_before;
    let pumped = dex.quote(pool, dex.wrap.id(), meme.id(), 10 * NEAR).await?;
    say!("a whale buys MEME with 300 wNEAR: 10 wNEAR now fetches only {} MEME", amt(pumped, 6, 0));
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 1})).await?;
    // 10 wNEAR's minimum is 10 x the floor per wNEAR.
    let floor = per_in * 10;
    assert!(r.logs().join("").contains("step_failed"), "{:?} {:?}", r.logs(), r.failures());
    assert!(pumped < floor);
    say!("keeper steps node 1: REFUSED. Alice's floor is {} MEME; Rhea reverted, nothing spent", amt(floor, 6, 0));
    assert_eq!(inbox(&ledger(&w, f).await?, 1), 10 * NEAR, "the wNEAR waits in node 1");
    dex.trade(&lp, pool, meme.id(), dex.wrap.id(), whale_got).await?;
    say!("the whale sells back; the pool is at the market again");
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 1})).await?;
    let got = u(&event(&r, "step")["out"]);
    let held = rhea::balance(&meme, &w.vault).await?;
    assert_eq!(held, got);
    assert!(got >= floor, "at least the floor");
    let fair = 10 * MEME_UNITS_PER_NEAR;
    say!("keeper steps node 1: 10 wNEAR -> {} MEME into the vault (market {}, {:.2}% under it: pool fee + impact)",
        amt(got, 6, 0), amt(fair, 6, 0), (fair - got) as f64 * 100.0 / fair as f64);
    show_ledger(&w, f, &[("wrap NEAR", (24, "NEAR"), (24, "wNEAR")), ("buy MEME", (24, "wNEAR"), (6, "MEME"))]).await?;

    // =====================================================================
    header("2. Buy a pump.fun memecoin on Solana (mainnet fork)");
    sol.rpc_call("surfnet_setAccount", json!([sol_me, {"lamports": 2_000_000_000u64}]));
    say!("the fork gives the vault's Solana address 2 SOL (a cheat code: no real money)");
    let probe = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, "50000000", "5"]);
    let per_lamport = probe["tokens"].as_str().unwrap_or_else(|| panic!("pump.fun helper: {probe}")).parse::<u128>()? / 52_500_000;
    // The owner's template: this coin only, at most 0.1 SOL a buy, at least half today's coins per lamport, into the vault's own account.
    let pump_buy = sol_template("pump_buy", &[("mint", PUMP_MINT), ("max_lamports", "100000000"), ("floor_per_lamport", &(per_lamport / 2).to_string())]);
    x(&w.alice, &w.vault, "sol_template", json!({"template": pump_buy})).await?.into_result()?;
    say!("Alice allows one template: pump.fun buy of {}…, max 0.1 SOL, floor {} units per lamport", &PUMP_MINT[..8], amt(per_lamport / 2, 0, 0));
    // A flow of one node holds the SOL the buy may spend: the keeper can't sign anything outside it.
    let pump_node = json!({"input": "sol:SOL", "when": {"when": "now"}, "next": [],
        "action": {"label": "Buy the coin", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "pump_buy", "amount_field": 1, "min_field": 0},
                       "out": format!("sol:{PUMP_MINT}"), "max_out_per_in": (per_lamport * 4 * E24).to_string()}}});
    let p: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "Buy the coin", "nodes": [pump_node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": p, "node": 0, "amount": "50000000"})).await?.into_result()?;
    say!("flow \"Buy the coin\": one node holding 0.05 SOL of the vault's SOL; Alice funds it");
    let coins = |s: &SolFork| u(&s.helper(&["balance", &sol_me, PUMP_MINT])["amount"]);
    let b = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, "50000000", "5", "exact"]);
    let r = x(&w.keeper, &w.vault, "sol_sign_tx", json!({"message": b["message"]})).await?;
    assert!(sol_sig(&r).is_none() && fails_with(&r, "owner only"), "{:?}", r.failures());
    say!("keeper asks the vault to sign the buy directly, outside any flow: REFUSED (owner only), nothing signed");
    let big = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, "60000000", "5", "exact"]);
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": p, "node": 0, "args": {"chain": "sol", "message": big["message"]}})).await?;
    assert!(sol_sig(&r).is_none() && fails_with(&r, "exactly"), "{:?}", r.failures());
    say!("keeper asks the node to sign a 0.06 SOL buy: REFUSED (the node spends exactly its 0.05 SOL), nothing signed");
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": p, "node": 0, "args": {"chain": "sol", "message": b["message"]}})).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = sol.helper(&["send", b["message"].as_str().unwrap(), &sig]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "not executed: {sent}");
    let pump_bought = coins(&sol);
    assert!(pump_bought >= u(&event(&r, "remote_step")["min_out"]));
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": p, "node": 0, "out": pump_bought.to_string()})).await?.into_result()?;
    say!("keeper builds a 0.05 SOL buy with pump.fun's SDK and steps the node; the vault checks every byte and signs");
    say!("executed on the fork: {} coins (6 decimals) in the vault's own token account, tx {}…; reported", amt(pump_bought, 6, 0), &sent["signature"].as_str().unwrap()[..16]);

    // =====================================================================
    header("3. Meteora LP on Solana, its fees swept into the memecoin by a flow");
    let owner = sol.helper(&["keypair"]);
    let (owner_key, owner_secret) = (owner["pubkey"].as_str().unwrap().to_string(), owner["secret"].as_str().unwrap().to_string());
    x(&w.alice, &w.vault, "sol_owner_wallet", json!({"wallet": owner_key})).await?.into_result()?;
    for name in ["dlmm_open", "dlmm_add_usdc", "dlmm_claim", "wsol_unwrap"] {
        x(&w.alice, &w.vault, "sol_template", json!({"template": sol_template(name, &[])})).await?.into_result()?;
    }
    sol.rpc_call("surfnet_setTokenAccount", json!([sol_me, USDC, {"amount": 500_000_000u64}, TOKEN]));
    say!("Alice's own Solana wallet {}… must co-sign any LP open; the vault holds 500 USDC on the fork", &owner_key[..8]);
    let alone = sol.helper(&["dlmm-open", &sol_me, "-", DLMM_POOL, "100000000", "10"]);
    let r = x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": alone["message"]})).await?;
    assert!(sol_sig(&r).is_none() && fails_with(&r, "Signers"), "{:?}", r.failures());
    say!("an open without Alice's wallet in it: REFUSED, even asked from her own NEAR account");
    let open = sol.helper(&["dlmm-open", &sol_me, &owner_key, DLMM_POOL, "100000000", "10"]);
    let (msg, position, pos_secret) = (open["message"].as_str().unwrap(), open["position"].as_str().unwrap().to_string(), open["positionSecret"].as_str().unwrap());
    let r = x(&w.alice, &w.vault, "sol_sign_tx", json!({"message": msg})).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("open not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = sol.helper(&["send", msg, &sig, pos_secret, &owner_secret]);
    assert!(sent["signature"].is_string() && sent["err"].is_null(), "open not executed: {sent}");
    say!("with Alice's co-signature: 100 USDC into 10 bins of Meteora's SOL/USDC DLMM pool, position {}…", &position[..8]);
    let trader = sol.helper(&["keypair"]);
    sol.rpc_call("surfnet_setAccount", json!([trader["pubkey"], {"lamports": 200_000_000_000u64}]));
    let swap = sol.helper(&["dlmm-swap", trader["secret"].as_str().unwrap(), DLMM_POOL, "100000000000"]);
    assert!(swap["err"].is_null() && swap["signature"].is_string(), "trader's swap: {swap}");
    let p = sol.helper(&["dlmm-position", DLMM_POOL, &position]);
    let fee_x = u(&p["feeX"]);
    assert!(fee_x > 0, "the position earned fees: {p}");
    say!("a trader sells 100 SOL through the pool: the position earns {} SOL in fees", amt(fee_x, 9, 9));

    // The flow: a collector node sweeps the fees (no input; the keeper reports what came, capped at 0.5 SOL a sweep),
    // and routes all of it to a buy node that must spend exactly that.
    let sweep = json!([
        {"input": "none", "when": {"when": "every", "every_ns": 3_600_000_000_000u64, "chunk": "1"}, "next": [{"to": 1, "bps": 10_000}],
         "action": {"label": "Sweep Meteora fees", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "dlmm_claim"}, "out": "sol:SOL", "max_out": "500000000"}}},
        {"input": "sol:SOL", "when": {"when": "now"}, "next": [],
         "action": {"label": "Buy the memecoin", "calls": [], "out": null, "min_out_per_in": null, "price": null,
            "remote": {"call": {"chain": "sol", "template": "pump_buy", "amount_field": 1, "min_field": 0},
                       "out": format!("sol:{PUMP_MINT}"), "max_out_per_in": (per_lamport * 4 * E24).to_string()}}}]);
    let g: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "LP fees into the coin", "nodes": sweep})).await?.json()?;
    say!("flow \"LP fees into the coin\": [sweep fees hourly, max 0.5 SOL] -> 100% -> [pump.fun buy, exactly what the sweep brought]");
    // Claim and unwrap in one transaction: the fees land as plain SOL, what pump.fun spends.
    let claim = sol.helper(&["dlmm-claim", &sol_me, DLMM_POOL, &position, "unwrap"]);
    let wsol_ata = sol.helper(&["ata", &sol_me, WSOL])["ata"].as_str().unwrap().to_string();
    let lamports = |s: &SolFork, who: &str| s.rpc_call("getBalance", json!([who]))["result"]["value"].as_u64().unwrap_or(0) as u128;
    let (before, ata_before) = (lamports(&sol, &sol_me), lamports(&sol, &wsol_ata));
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": g, "node": 0, "args": {"chain": "sol", "message": claim["message"]}})).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("claim not signed: {:?} {:?}", r.logs(), r.failures()));
    let sent = sol.helper(&["send", claim["message"].as_str().unwrap(), &sig]);
    assert!(sent["err"].is_null() && sent["signature"].is_string(), "claim not executed: {sent}");
    let tx = sol.rpc_call("getTransaction", json!([sent["signature"], {"encoding": "json", "maxSupportedTransactionVersion": 0, "commitment": "confirmed"}]));
    let tx_fee = tx["result"]["meta"]["fee"].as_u64().unwrap_or_else(|| panic!("no fee in {tx}")) as u128;
    // What arrived: the vault's SOL gain, its fee added back, the wSOL account's rent (returned by the close) taken out.
    let fees = lamports(&sol, &sol_me) + tx_fee - before - ata_before;
    say!("keeper steps the sweep: claim_fee2 + close wSOL, checked and signed by the vault, executed: +{} SOL", amt(fees, 9, 9));
    assert!(fees.abs_diff(fee_x) * 100 <= fee_x, "measured {fees} vs the position's {fee_x}");
    assert!(fails_with(&x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": g, "node": 0, "out": "600000000"})).await?, "over the node's cap"));
    say!("keeper reports 0.6 SOL of fees: REFUSED (over the node's 0.5 SOL cap)");
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": g, "node": 0, "out": fees.to_string()})).await?.into_result()?;
    assert_eq!(inbox(&ledger(&w, g).await?, 1), fees, "all of it routed to the buy");
    say!("keeper reports {} SOL: accepted, routed 100% to the buy node", amt(fees, 9, 9));
    show_ledger(&w, g, &[("sweep fees", (9, "-"), (9, "SOL")), ("buy the coin", (9, "SOL"), (6, "COIN"))]).await?;
    let more = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, &(fees + 1).to_string(), "5", "exact"]);
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": g, "node": 1, "args": {"chain": "sol", "message": more["message"]}})).await?;
    assert!(fails_with(&r, "exactly"), "{:?}", r.failures());
    say!("keeper asks the buy node to spend 1 lamport more than it holds: REFUSED");
    let b = sol.helper(&["pump-buy", &sol_me, PUMP_MINT, &fees.to_string(), "5", "exact"]);
    let held_before = coins(&sol);
    let r = x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": g, "node": 1, "args": {"chain": "sol", "message": b["message"]}})).await?;
    let sig = sol_sig(&r).unwrap_or_else(|| panic!("buy not signed: {:?} {:?}", r.logs(), r.failures()));
    let min = u(&event(&r, "remote_step")["min_out"]);
    let sent = sol.helper(&["send", b["message"].as_str().unwrap(), &sig]);
    assert!(sent["err"].is_null() && sent["signature"].is_string(), "buy not executed: {sent}");
    let swept = coins(&sol) - held_before;
    assert_eq!(swept, min, "pump.fun's buy takes exactly the coins asked for");
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": g, "node": 1, "out": swept.to_string()})).await?.into_result()?;
    say!("keeper steps the buy: {} SOL of fees -> {} coins on the fork (signed minimum {}), reported", amt(fees, 9, 9), amt(swept, 6, 0), amt(min, 6, 0));
    let l = ledger(&w, g).await?;
    assert_eq!((inbox(&l, 0), inbox(&l, 1), u(&l["nodes"][1]["out_total"])), (0, 0, swept));
    show_ledger(&w, g, &[("sweep fees", (9, "-"), (9, "SOL")), ("buy the coin", (9, "SOL"), (6, "COIN"))]).await?;
    say!("the vault now holds {} coins in all", amt(coins(&sol), 6, 0));

    // =====================================================================
    header("4. Robinhood Chain: NVDA take-profit ladder into the ETHNVDA basket");
    for t in common::templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    rh.cast(&["rpc", "anvil_setBalance", &evm_me, "0xDE0B6B3A7640000"]); // 1 ETH
    let eth_of = |f: &Fork| num(&f.cast(&["balance", &evm_me]));
    let max_fee = (num(&rh.cast(&["base-fee"])) * 2).max(GWEI / 100).min(GWEI);
    let nonce = |f: &Fork| num(&f.cast(&["nonce", &evm_me])) as u64;
    let args = |values: Vec<String>, nonce: u64| json!({"chain": "evm", "values": values, "nonce": nonce, "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 1_000_000u64});
    say!("Robinhood Chain fork at block {}; the vault's address gets 1 ETH", rh.cast(&["block-number"]));
    // Once: let Uniswap's Universal Router pull NVDA through Permit2 (Alice's own template calls, outside the flow).
    let now: u128 = num(&rh.cast(&["block", "latest", "--field", "timestamp"]));
    for (name, values) in [("approve_nvda_permit2", vec![0, E18]), ("permit2_nvda_universal_router", vec![0, 0, E18, now + 86_400])] {
        let a = json!({"name": name, "values": values.iter().map(|v| v.to_string()).collect::<Vec<_>>(), "nonce": nonce(&rh), "max_fee": max_fee.to_string(), "max_tip": "0", "gas": 200_000u64});
        let r = x(&w.alice, &w.vault, "evm_call", a).await?;
        assert_eq!(send(&rh, &signed(&r)).0, 1, "{name}");
    }
    set_pyth(&w, &pyth, NVDA_FEED, 180 * 100_000_000, 0).await?;
    let h: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "NVDA ladder", "nodes": ladder::ladder()})).await?.json()?;
    let eth_in = 30_000_000_000_000_000u128;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": h, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    say!("flow \"NVDA ladder\": [buy NVDA] -> 1/3 each -> [sell at $270] [sell at $360] [sell at $540] -> [zap into ETHNVDA]");
    say!("NVDA at $180 (Pyth); Alice funds the ladder with 0.03 ETH");
    let step = |node: u16, a: Value| x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": h, "node": node, "args": a}));
    let q = v4_quote(&rh, true, eth_in);
    let r = step(0, args(v4_values(q * 99 / 100, true), nonce(&rh))).await?;
    assert_eq!(send(&rh, &signed(&r)).0, 1, "buy executed");
    let bought = rh.balance(NVDA, &evm_me);
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": h, "node": 0, "out": bought.to_string()})).await?.into_result()?;
    let third = bought * 3_333 / 10_000;
    say!("buy (Uniswap v4): 0.03 ETH -> {} NVDA on the fork; reported, a third to each rung", amt(bought, 18, 6));
    let mut basket = 0u128;
    let mut eth_back = 0u128;
    for (rung, usd) in [(1u16, 275i64), (2, 365), (3, 545)] {
        let at = 180 * [15, 20, 30][rung as usize - 1] / 10;
        set_pyth(&w, &pyth, NVDA_FEED, (at - 1) * 100_000_000, 0).await?;
        let r = step(rung, args(v4_values(1, false), nonce(&rh))).await?;
        assert!(fails_with(&r, "NotYet"), "rung {rung} below its price: {:?}", r.failures());
        set_pyth(&w, &pyth, NVDA_FEED, usd * 100_000_000, 0).await?;
        let q = v4_quote(&rh, false, third);
        let before = eth_of(&rh);
        let r = step(rung, args(v4_values(q * 99 / 100, false), nonce(&rh))).await?;
        let (st, gas) = send(&rh, &signed(&r));
        assert_eq!(st, 1, "rung {rung} executed");
        let got = eth_of(&rh) + gas - before;
        x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": h, "node": rung, "out": got.to_string()})).await?.into_result()?;
        eth_back += got;
        let shares_before = rh.balance(ETHNVDA, &evm_me);
        let r = step(4, args(vec!["0".into(); 5], nonce(&rh))).await?;
        assert_eq!(event(&r, "evm_call_requested")["value"], json!(got.to_string()), "the zap spends exactly the sale's ETH");
        assert_eq!(send(&rh, &signed(&r)).0, 1, "zap executed");
        let shares = rh.balance(ETHNVDA, &evm_me) - shares_before;
        x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": h, "node": 4, "out": shares.to_string()})).await?.into_result()?;
        basket += shares;
        say!("NVDA ${}: rung {rung} refused (below ${at}). NVDA ${usd}: sold {} NVDA -> {} ETH -> {} ETHNVDA shares",
            at - 1, amt(third, 18, 6), amt(got, 18, 6), amt(shares, 18, 6));
    }
    let l = ledger(&w, h).await?;
    for i in 0..5 {
        assert_eq!(inbox(&l, i), 0, "node {i} empty");
    }
    assert_eq!(rh.balance(ETHNVDA, &evm_me), basket, "the fork agrees with the ledger");
    assert_eq!(u(&l["nodes"][4]["out_total"]), basket);
    show_ledger(&w, h, &[("buy NVDA", (18, "ETH"), (18, "NVDA")), ("sell at $270", (18, "NVDA"), (18, "ETH")), ("sell at $360", (18, "NVDA"), (18, "ETH")), ("sell at $540", (18, "NVDA"), (18, "ETH")), ("into ETHNVDA", (18, "ETH"), (18, "shares"))]).await?;
    say!("ladder done: 0.03 ETH -> {} NVDA -> {} ETH -> {} ETHNVDA shares (fork balance = ledger)", amt(bought, 18, 6), amt(eth_back, 18, 6), amt(basket, 18, 6));

    // =====================================================================
    header("Done");
    say!("NEAR:     10 NEAR -> {} MEME on Rhea; a pumped pool was refused by the oracle floor", amt(held, 6, 0));
    say!("Solana:   0.05 SOL -> {} pump.fun coins; Meteora fees {} SOL -> {} more, routed by the flow", amt(pump_bought, 6, 0), amt(fees, 9, 9), amt(swept, 6, 0));
    say!("Robinhood: 0.03 ETH -> NVDA -> three take-profits -> {} ETHNVDA shares", amt(basket, 18, 6));
    say!("one vault, three chains, {:.0} s", started.elapsed().as_secs_f64());
    Ok(())
}
