//! Pyth prices checked on NEAR by our own board (priceboard, source "pyth_core"):
//! anyone posts the update exactly as Hermes serves it, the board checks Pyth's
//! signers (a majority of its signer set), the emitter and each price's Merkle
//! proof, and the brain reads it through Pyth's interface as before. A market
//! floor on an ETH -> USDC swap then runs off those prices, each confirmed by its
//! Chainlink backup (on an attested board, as `npm run chainlink-price -- --board`
//! posts it).
//!
//! The updates here are built in Hermes's exact format (PNAU, a VAA over a
//! keccak160 Merkle root, depth-12 proofs, 85-byte price messages, 3 of 5
//! signatures: the shape of today's real ones), at the sandbox's clock and signed
//! by 5 test keys, because the vault wants prices under 60 s old and Pyth's
//! signers don't sign for a test. Real updates with Pyth's real signers are
//! checked in priceboard's unit tests (tests-data/pyth_core.json); the last test
//! here posts one too, to measure what verifying real signatures costs.
//! Build the wasm first: make build

mod board;
mod common;
mod relayw;

use board::{attester, near_key, now};
use common::wasm;
use k256::ecdsa::SigningKey as Secp;
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract, Worker};
use relayw::*;
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const E24: u128 = 1_000_000_000_000_000_000_000_000;
const SOL_FEED: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";
/// Chainlink ETH/USD and USDC/USD on Arbitrum (23): the backups.
const ETH_ARB: &str = "001700000000000000000000639fe6ab55c921f74e7fac1ee960c0b6293ba612";
const USDC_ARB: &str = "00170000000000000000000050834f3163758fcc1df9973b6e91f0f0f0434ad3";
/// Pythnet's emitter ("PythnetPythnet..."), chain 26.
const PYTHNET: &str = "507974686e6574507974686e6574507974686e6574507974686e657450797468";

fn signer(i: u8) -> Secp {
    Secp::from_slice(&[i + 41; 32]).unwrap()
}

fn signer_addr(k: &Secp) -> String {
    let pt = k.verifying_key().to_encoded_point(false);
    hex::encode(&evmtx::keccak256(&pt.as_bytes()[1..])[12..])
}

fn k160(parts: &[&[u8]]) -> [u8; 20] {
    evmtx::keccak256(&parts.concat())[..20].try_into().unwrap()
}

fn node(a: [u8; 20], b: [u8; 20]) -> [u8; 20] {
    if a <= b { k160(&[&[1], &a, &b]) } else { k160(&[&[1], &b, &a]) }
}

/// A Pyth price message (type 0): id, price, conf, expo, publish time, previous time, EMA price and conf.
fn message(id: &str, price: i64, conf: u64, t: i64) -> Vec<u8> {
    let mut m = vec![0u8];
    m.extend(hex::decode(id).unwrap());
    m.extend(price.to_be_bytes());
    m.extend(conf.to_be_bytes());
    m.extend((-8i32).to_be_bytes());
    m.extend(t.to_be_bytes());
    m.extend((t - 1).to_be_bytes());
    m.extend(price.to_be_bytes());
    m.extend(conf.to_be_bytes());
    m
}

/// An accumulator update in Hermes's format: up to 4 leaves under a depth-12 tree
/// (2 real levels, 10 more with fixed siblings, as deep as today's real proofs),
/// the root in an "AUWV" payload from Pythnet, signed by test signers 0, 1, 2 of 5 (set 1).
fn update(msgs: &[Vec<u8>], t: i64) -> Vec<u8> {
    assert!(msgs.len() <= 4);
    let mut leaves: Vec<[u8; 20]> = msgs.iter().map(|m| k160(&[&[0], m])).collect();
    while leaves.len() < 4 {
        leaves.push([0xab; 20]);
    }
    let (l, r) = (node(leaves[0], leaves[1]), node(leaves[2], leaves[3]));
    let mut root = node(l, r);
    let high: Vec<[u8; 20]> = (0..10u8).map(|i| [i + 1; 20]).collect();
    for s in &high {
        root = node(root, *s);
    }
    let mut payload = b"AUWV".to_vec();
    payload.push(0);
    payload.extend(7_000_000u64.to_be_bytes()); // slot
    payload.extend(0u32.to_be_bytes()); // ring size
    payload.extend(root);
    let mut body = Vec::new();
    body.extend((t as u32).to_be_bytes());
    body.extend(0u32.to_be_bytes());
    body.extend(26u16.to_be_bytes());
    body.extend(hex::decode(PYTHNET).unwrap());
    body.extend((t as u64).to_be_bytes()); // sequence
    body.push(0);
    body.extend(payload);
    let digest = evmtx::keccak256(&evmtx::keccak256(&body));
    let mut vaa = vec![1u8];
    vaa.extend(1u32.to_be_bytes());
    vaa.push(3);
    for i in 0..3u8 {
        let (s, rec) = signer(i).sign_prehash_recoverable(&digest).unwrap();
        vaa.push(i);
        vaa.extend(s.to_bytes());
        vaa.push(rec.to_byte());
    }
    vaa.extend(body);
    let mut u = b"PNAU".to_vec();
    u.extend([1, 0, 0, 0]); // major, minor, no trailing header, Wormhole Merkle
    u.extend((vaa.len() as u16).to_be_bytes());
    u.extend(vaa);
    u.push(msgs.len() as u8);
    for (i, m) in msgs.iter().enumerate() {
        u.extend((m.len() as u16).to_be_bytes());
        u.extend(m);
        u.push(12);
        u.extend(leaves[i ^ 1]);
        u.extend(if i < 2 { r } else { l });
        for s in &high {
            u.extend(s);
        }
    }
    u
}

/// ETH $2100.78680519 +-$2.63, SOL $86.05, USDC $0.99974372 (today's real Hermes prices) at time `t`.
fn prices(t: i64) -> Vec<Vec<u8>> {
    vec![message(ETH_FEED, 210_078_680_519, 262_819_472, t), message(SOL_FEED, 8_605_169_617, 2_615_046, t), message(USDC_FEED, 99_974_372, 12_288, t)]
}

/// A PythCore board on the 5 test signers (set 1), listing ETH, SOL, USDC.
async fn pyth_board(w: &Worker<Sandbox>, owner: &Account, signers: Vec<String>, index: u32) -> anyhow::Result<Contract> {
    let b = w.dev_deploy(&wasm("priceboard")).await?;
    b.call("new").args_json(json!({"owner": owner.id(), "source": {"kind": "pyth_core", "guardian_set": index, "guardians": signers,
        "emitter_chain": 26, "emitter": PYTHNET, "feeds": [ETH_FEED, SOL_FEED, USDC_FEED], "max_age_s": 60}}))
        .transact().await?.into_result()?;
    Ok(b)
}

async fn post(who: &Account, board: &Contract, u: &[u8]) -> anyhow::Result<near_workspaces::result::ExecutionFinalResult> {
    Ok(who.call(board.id(), "post_update").args_json(json!({"data": hex::encode(u)})).max_gas().transact().await?)
}

/// The Chainlink backups (ETH $2100.50, USDC $0.9998) on an attested board, as chainlink-price --board attests them.
async fn post_backups(w: &Worker<Sandbox>, who: &Account, att: &Contract) -> anyhow::Result<()> {
    use ed25519_dalek::Signer;
    let t = now(w).await?;
    for (id, price) in [(ETH_ARB, 210_050_000_000i64), (USDC_ARB, 99_980_000)] {
        let m = board::message(id, price, -8, t);
        let report = json!({"attester": near_key(&attester(0)), "price": price.to_string(), "expo": -8, "publish_time": t, "signature": hex::encode(attester(0).sign(&m).to_bytes())});
        who.call(att.id(), "push").args_json(json!({"feed": id, "reports": [report]})).max_gas().transact().await?.into_result()?;
    }
    Ok(())
}

#[tokio::test]
async fn a_market_floor_runs_off_pyth_prices_the_board_checked_itself() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let signers: Vec<String> = (0..5).map(|i| signer_addr(&signer(i))).collect();
    let pyth = pyth_board(&w.worker, &w.alice, signers, 1).await?;
    let att = w.worker.dev_deploy(&wasm("priceboard")).await?;
    att.call("new").args_json(json!({"owner": w.alice.id(), "source": {"kind": "attested", "attesters": [near_key(&attester(0))], "k": 1, "max_skew_s": 60}}))
        .transact().await?.into_result()?;
    let anyone = w.worker.dev_create_account().await?;

    // Gas: one feed, then three, in one update each (3 signatures checked either way).
    let t = now(&w.worker).await?;
    let r = post(&anyone, &pyth, &update(&prices(t)[..1], t)).await?;
    println!("GAS post_update (1 feed, 3 of 5 signatures, depth-12 proof): {:.2} Tgas", tgas(&r));
    assert!(r.logs().iter().any(|l| l.contains(r#""price":"210078680519""#)), "{:?} {:?}", r.logs(), r.failures());
    r.into_result()?;
    while now(&w.worker).await? <= t {
        w.worker.fast_forward(1).await?;
    }
    let t = now(&w.worker).await?;
    let r = post(&anyone, &pyth, &update(&prices(t), t)).await?;
    println!("GAS post_update (3 feeds, 3 of 5 signatures, depth-12 proofs): {:.2} Tgas", tgas(&r));
    let out: Vec<Value> = r.into_result()?.json()?;
    assert_eq!(out.len(), 3);
    let p: Value = pyth.view("get_price_no_older_than").args_json(json!({"price_id": USDC_FEED, "age": 60})).await?.json()?;
    assert_eq!((p["price"].as_str(), p["conf"].as_str(), p["expo"].as_i64(), p["publish_time"].as_i64()), (Some("99974372"), Some("12288"), Some(-8), Some(t)), "Pyth's own price and conf");

    // What the board refuses, through the real wasm: 2 signatures; another emitter's root
    // (signed by the same keys); a price with a changed byte (its proof no longer holds).
    let u = update(&prices(t + 1), t + 1);
    let vlen = u16::from_be_bytes([u[8], u[9]]) as usize;
    let vaa = &u[10..10 + vlen];
    let two = [&u[..8], &((vlen - 66) as u16).to_be_bytes(), &vaa[..5], &[2], &vaa[6 + 66..], &u[10 + vlen..]].concat();
    assert!(fails_with(&post(&anyone, &pyth, &two).await?, "no quorum"));
    let mut forged = u.clone();
    forged[10 + vlen + 1 + 2 + 40] ^= 1;
    assert!(fails_with(&post(&anyone, &pyth, &forged).await?, "bad Merkle proof"));
    let stranger: Vec<String> = (5..10).map(|i| signer_addr(&signer(i))).collect();
    let other = pyth_board(&w.worker, &w.alice, stranger, 1).await?;
    assert!(fails_with(&post(&anyone, &other, &u).await?, "bad guardian signature"), "not the board's signers");

    // ---- the vault: ETH and USDC from the Pyth board, each backed by Chainlink on the attested board ----
    let other = |id: &str| json!({"kind": "other", "contract": att.id(), "price_id": id});
    let feed = |name: &str, id: &str, backup: Value| json!({"name": name, "contract": pyth.id(), "price_id": id, "backup": backup});
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200, "feeds": [
        feed("eth", ETH_FEED, other(ETH_ARB)), feed("usdc", USDC_FEED, other(USDC_ARB))]}})).await?.into_result()?;

    let minter = w.worker.dev_create_account().await?;
    let mk = |sym: &'static str| {
        let (worker, minter) = (w.worker.clone(), minter.clone());
        async move {
            let t = worker.dev_deploy(&wasm("mock_ft")).await?;
            t.call("new").args_json(json!({"owner_id": minter.id(), "total_supply": (1_000_000_000 * ONE).to_string(), "name": sym, "symbol": sym, "decimals": 18, "call_fee_bps": 0}))
                .transact().await?.into_result()?;
            anyhow::Ok(t)
        }
    };
    let (weth, usdc) = (mk("WETH").await?, mk("USDC").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": weth.id()})).transact().await?.into_result()?;
    for tk in [&weth, &usdc] {
        for who in [dex.id(), &w.vault] {
            minter.call(tk.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    minter.call(usdc.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    minter.call(weth.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (10 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": weth.id()}, {"contract": usdc.id()}, {"contract": dex.id()}], "remove": []})).await?.into_result()?;
    let node = json!({"input": weth.id(), "when": {"when": "now"}, "action": {"label": "Sell ETH at market", "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, usdc.id())}], "out": usdc.id(), "min_out_per_in": null, "price": null,
        "market": {"sell": "eth", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "sell", "nodes": [node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": ONE.to_string()})).await?.into_result()?;
    let rate = |r: u128| {
        let (a, d, i, o) = (w.alice.clone(), dex.clone(), weth.id().clone(), usdc.id().clone());
        async move { a.call(d.id(), "set_rate").args_json(json!({"token_in": i, "token_out": o, "rate": r.to_string()})).transact().await?.into_result()?; anyhow::Ok(()) }
    };
    // Fresh prices just before the step, as the keeper posts them (Pyth's update, then the backups).
    let fresh = |anyone: Account| {
        let (wk, pyth, att) = (w.worker.clone(), pyth.clone(), att.clone());
        async move {
            let t = now(&wk).await?;
            let _ = post(&anyone, &pyth, &update(&prices(t), t)).await?; // refused if the same second: not newer
            post_backups(&wk, &anyone, &att).await?;
            anyhow::Ok(())
        }
    };
    // Floor per ETH = $2100.78680519 / $0.99974372 x 0.99 = 2080.3121... USDC (Python fractions, x 1e24, floored):
    // 2080312079517838831735797250. A pool paying 2000 is refused, nothing sold.
    fresh(anyone.clone()).await?;
    rate(2000 * E24).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let logs = r.logs().join("\n");
    assert!(logs.contains(r#""min_out_per_in":"2080312079517838831735797250""#) && logs.contains("step_failed"), "{logs} {:?}", r.failures());
    // A pool at the market (2100): sold, 2100 USDC.
    fresh(anyone.clone()).await?;
    rate(2100 * E24).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    println!("GAS relay step with a market floor read off a pyth_core board: {:.1} Tgas", tgas(&r));
    assert!(r.logs().join("").contains(r#""out":"2100000000000000000000""#), "{:?} {:?}", r.logs(), r.failures());

    // No fresh Pyth price, no step: a minute on, the board's price is too old for the vault.
    let t = now(&w.worker).await?;
    while now(&w.worker).await? <= t + 61 {
        w.worker.fast_forward(50).await?;
    }
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": ONE.to_string()})).await?.into_result()?;
    post_backups(&w.worker, &anyone, &att).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    assert!(fails_with(&r, "no price fresher than 60 s"), "{:?}", r.failures());
    Ok(())
}

#[tokio::test]
async fn a_real_hermes_update_costs_what_a_test_one_does() -> anyhow::Result<()> {
    // Real bytes, Pyth's real signers: the board on Pyth's set 1 checks everything,
    // then refuses the price as stale (it was captured on 2026-11-09, the sandbox's
    // clock is now). The gas of that refusal is the gas of the checks.
    let w = near_workspaces::sandbox().await?;
    let owner = w.dev_create_account().await?;
    let j: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../priceboard/tests-data/pyth_core.json", env!("CARGO_MANIFEST_DIR")))?)?;
    let set1: Vec<String> = j["guardian_set_1"].as_array().unwrap().iter().map(|g| g.as_str().unwrap().to_string()).collect();
    let board = pyth_board(&w, &owner, set1, 1).await?;
    for (name, n) in [("eth", 1), ("eth_sol_usdc", 3)] {
        let u = hex::decode(j["updates"][name]["hex"].as_str().unwrap())?;
        let r = post(&owner, &board, &u).await?;
        assert!(fails_with(&r, "s old, at most 60"), "{name}: all checks passed, then stale: {:?}", r.failures());
        println!("GAS post_update, real Hermes update ({n} feed(s), Pyth's 3 real signatures), all checks then refused as stale: {:.2} Tgas", tgas(&r));
    }
    // With PYTH_API_KEY set: today's update, live from Hermes, taken whole (the sandbox's clock is real time).
    let Ok(key) = std::env::var("PYTH_API_KEY") else {
        println!("PYTH_API_KEY not set: no live Hermes update posted");
        return Ok(());
    };
    use std::io::Write;
    let url = format!("https://pyth.dourolabs.app/hermes/v2/updates/price/latest?ids[]={ETH_FEED}&ids[]={SOL_FEED}&ids[]={USDC_FEED}&encoding=hex");
    // The key goes to curl on stdin, never on its command line.
    let mut curl = std::process::Command::new("curl").args(["-s", "--config", "-", &url]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn()?;
    curl.stdin.take().unwrap().write_all(format!("header = \"authorization: Bearer {key}\"\n").as_bytes())?;
    let live: Value = serde_json::from_slice(&curl.wait_with_output()?.stdout)?;
    let u = hex::decode(live["binary"]["data"][0].as_str().unwrap())?;
    let r = post(&owner, &board, &u).await?;
    println!("GAS post_update, LIVE Hermes update (3 feeds, Pyth's real signers), stored: {:.2} Tgas", tgas(&r));
    let out: Vec<Value> = r.into_result()?.json()?;
    assert_eq!(out.len(), 3);
    let p: Value = board.view("get_price_no_older_than").args_json(json!({"price_id": ETH_FEED, "age": 60})).await?.json()?;
    println!("live ETH on the board: {} x 1e{} +-{}, published {}", p["price"], p["expo"], p["conf"], p["publish_time"]);
    Ok(())
}
