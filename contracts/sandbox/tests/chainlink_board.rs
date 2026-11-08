//! Prices from other chains drive a relay vault: NEAR/USD (Chainlink on
//! Arbitrum) and NVDA (Chainlink RHNVDA/USD on Robinhood Chain) reach an Evm
//! price board as a Wormhole Query (eth_call) response, USDC reaches an attested
//! board the way `npm run pyth -- --board` relays Hermes; the brain reads both
//! through Pyth's interface. A market floor on a NEAR -> USDC swap and a rung of
//! the NVDA take-profit ladder then run off those prices.
//!
//! The Chainlink answers are the real bytes read on 2026-11-06
//! (priceboard/tests-data/chainlink_reads.json: NEAR $3.75297364, NVDA
//! $189.55682289); only the block time and updatedAt are moved to the sandbox's
//! clock (same 528 s gap), and the guardians are test keys (a real signed
//! response needs WORMHOLE_QUERY_API_KEY; real signatures are checked in
//! priceboard's unit tests). Build the wasm first: make build

mod board;
mod common;
mod ladder;
mod relayw;

use board::{attester, near_key, now};
use common::wasm;
use k256::ecdsa::SigningKey as Secp;
use ladder::{event, evm_args, inbox, ladder as nvda_ladder, v4_values, E24, GWEI};
use near_workspaces::network::Sandbox;
use near_workspaces::types::NearToken;
use near_workspaces::{Contract, Worker};
use relayw::*;
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR_ID: &str = "001700000000000000000000bf5c3fb2633e924598a46b9d07a174a9dbcf57c0";
const NVDA_ID: &str = "004800000000000000000000379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15";
const USDC_ID: &str = "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a";

fn guardian(i: u8) -> Secp {
    Secp::from_slice(&[i + 1; 32]).unwrap()
}

fn guardian_addr(k: &Secp) -> String {
    let pt = k.verifying_key().to_encoded_point(false);
    hex::encode(&evmtx::keccak256(&pt.as_bytes()[1..])[12..])
}

fn var(out: &mut Vec<u8>, b: &[u8]) {
    out.extend((b.len() as u32).to_be_bytes());
    out.extend(b);
}

/// A recorded Chainlink read, moved to block time `t` (updatedAt keeps its real gap).
struct Read {
    chain: u16,
    address: Vec<u8>,
    block: u64,
    round: Vec<u8>,
    decimals: Vec<u8>,
}

fn read(name: &str, t: i64) -> Read {
    let j: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../priceboard/tests-data/chainlink_reads.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    let f = &j["feeds"][name];
    let h = |k: &str| hex::decode(f[k].as_str().unwrap().trim_start_matches("0x")).unwrap();
    let mut round = h("latestRoundData");
    let gap = f["block_time"].as_u64().unwrap() - u64::from_be_bytes(round[120..128].try_into().unwrap());
    round[120..128].copy_from_slice(&(t as u64 - gap).to_be_bytes()); // updatedAt: word 3
    Read { chain: f["wormhole_chain"].as_u64().unwrap() as u16, address: h("address"), block: f["block_number"].as_u64().unwrap(), round, decimals: h("decimals") }
}

/// An eth_call query response (proxy format): per read, latestRoundData() then decimals(), at time `t`.
fn response(reads: &[Read], t: i64) -> Vec<u8> {
    let mut req = vec![1u8];
    req.extend(7u32.to_be_bytes());
    req.push(reads.len() as u8);
    let mut resp = vec![reads.len() as u8];
    for r in reads {
        let mut q = Vec::new();
        var(&mut q, format!("0x{:x}", r.block).as_bytes());
        q.push(2);
        let mut a = Vec::new();
        a.extend(r.block.to_be_bytes());
        a.extend([5u8; 32]);
        a.extend((t as u64 * 1_000_000).to_be_bytes());
        a.push(2);
        for (sel, result) in [([0xfe, 0xaf, 0x96, 0x8c], &r.round), ([0x31, 0x3c, 0xe5, 0x67], &r.decimals)] {
            q.extend(&r.address);
            var(&mut q, &sel);
            var(&mut a, result);
        }
        req.extend(r.chain.to_be_bytes());
        req.push(1);
        var(&mut req, &q);
        resp.extend(r.chain.to_be_bytes());
        resp.push(1);
        var(&mut resp, &a);
    }
    let mut out = vec![1u8, 0, 0];
    out.extend([0u8; 65]);
    var(&mut out, &req);
    out.extend(resp);
    out
}

/// Signatures by test guardians 0, 1, 2 (proxy format: r||s||v||index, hex).
fn sign(resp: &[u8]) -> Vec<String> {
    let mut m = b"query_response_0000000000000000000|".to_vec();
    m.extend(evmtx::keccak256(resp));
    let d = evmtx::keccak256(&m);
    (0..3u8).map(|i| {
        let (s, rec) = guardian(i).sign_prehash_recoverable(&d).unwrap();
        let mut b = s.to_bytes().to_vec();
        b.extend([rec.to_byte(), i]);
        hex::encode(b)
    }).collect()
}

/// The Evm board with both feeds (0.5% deviation, 24 h heartbeat), and the attested one (one attester).
async fn boards(w: &Worker<Sandbox>, owner: &near_workspaces::Account) -> anyhow::Result<(Contract, Contract)> {
    let evm = w.dev_deploy(&wasm("priceboard")).await?;
    let feed = |chain: u16, a: &str| json!({"chain": chain, "address": a, "deviation_bps": 50, "heartbeat_s": 86_400});
    evm.call("new").args_json(json!({"owner": owner.id(), "ema_window_s": 600, "source": {"kind": "evm", "guardian_set": 0,
        "guardians": (0..3).map(|i| guardian_addr(&guardian(i))).collect::<Vec<_>>(), "max_block_age_s": 60,
        "feeds": [feed(23, "0xbf5c3fb2633e924598a46b9d07a174a9dbcf57c0"), feed(72, "0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15")]}}))
        .transact().await?.into_result()?;
    let att = w.dev_deploy(&wasm("priceboard")).await?;
    att.call("new").args_json(json!({"owner": owner.id(), "ema_window_s": 600, "source": {"kind": "attested", "attesters": [near_key(&attester(0))], "k": 1, "max_skew_s": 60}}))
        .transact().await?.into_result()?;
    Ok((evm, att))
}

/// Post both Chainlink feeds at the sandbox's clock; returns that time.
async fn post_chainlink(w: &Worker<Sandbox>, who: &near_workspaces::Account, evm: &Contract) -> anyhow::Result<i64> {
    let t = now(w).await?;
    let resp = response(&[read("near_arbitrum", t), read("nvda_robinhood", t)], t);
    let r = who.call(evm.id(), "push_evm").args_json(json!({"response": hex::encode(&resp), "signatures": sign(&resp)})).max_gas().transact().await?;
    println!("GAS push_evm (2 feeds, 3 guardians): {:.1} Tgas", tgas(&r));
    assert!(r.logs().iter().any(|l| l.contains(r#""round_id":"36893488147419122532""#)), "the source is in the log: {:?} {:?}", r.logs(), r.failures());
    r.into_result()?;
    Ok(t)
}

/// USDC at $0.99981068 as `npm run pyth -- --board` relays it (Pyth's id, Pyth's expo).
async fn post_usdc(w: &Worker<Sandbox>, who: &near_workspaces::Account, att: &Contract) -> anyhow::Result<()> {
    use ed25519_dalek::Signer;
    let t = now(w).await?;
    let m = board::message(USDC_ID, 99_981_068, -8, t);
    let report = json!({"attester": near_key(&attester(0)), "price": "99981068", "expo": -8, "publish_time": t, "signature": hex::encode(attester(0).sign(&m).to_bytes())});
    who.call(att.id(), "push").args_json(json!({"feed": USDC_ID, "reports": [report]})).max_gas().transact().await?.into_result()?;
    Ok(())
}

#[tokio::test]
async fn a_market_floor_and_a_ladder_rung_run_off_chainlink_prices_on_the_board() -> anyhow::Result<()> {
    let w = relay_world().await?;
    let (evm, att) = boards(&w.worker, &w.alice).await?;
    let anyone = w.worker.dev_create_account().await?;
    // Every ~150 s for 5 minutes: the history each board's average needs before it vouches for a price.
    board::history(&w.worker, |_| async {
        post_chainlink(&w.worker, &anyone, &evm).await?;
        post_usdc(&w.worker, &anyone, &att).await
    })
    .await?;
    let p: Value = evm.view("get_price_no_older_than").args_json(json!({"price_id": NEAR_ID, "age": 60})).await?.json()?;
    assert_eq!((p["price"].as_str(), p["conf"].as_str(), p["expo"].as_i64()), (Some("375297364"), Some("1876486"), Some(-8)), "NEAR $3.75297364 +-0.5%");

    // The vault's feeds: NEAR and NVDA from the Evm board, USDC from the attested one; each confirmed by its board's EMA.
    let feed = |name: &str, board: &Contract, id: &str| json!({"name": name, "contract": board.id(), "price_id": id, "backup": {"kind": "ema"}});
    x(&w.alice, &w.vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 200,
        "feeds": [feed("near", &evm, NEAR_ID), feed("usdc", &att, USDC_ID), feed("nvda", &evm, NVDA_ID)]}})).await?.into_result()?;

    // ---- a market floor: sell 10 wNEAR for USDC at most 1% under the board's price ----
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
    let (wnear, usdc) = (mk("wNEAR").await?, mk("USDC").await?);
    let dex = w.worker.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": wnear.id()})).transact().await?.into_result()?;
    for t in [&wnear, &usdc] {
        for who in [dex.id(), &w.vault] {
            minter.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    minter.call(usdc.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (1_000_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    minter.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": w.vault, "amount": (100 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    x(&w.alice, &w.vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": usdc.id()}, {"contract": dex.id()}], "remove": []})).await?.into_result()?;
    let node = json!({"input": wnear.id(), "when": {"when": "now"}, "action": {"label": "Sell NEAR at market", "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50,
        "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, usdc.id())}], "out": usdc.id(), "min_out_per_in": null, "price": null,
        "market": {"sell": "near", "buy": "usdc", "dec_in": 18, "dec_out": 18, "slippage_bps": 100}}});
    let f: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "sell", "nodes": [node]})).await?.json()?;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": f, "node": 0, "amount": (10 * ONE).to_string()})).await?.into_result()?;
    let rate = |r: u128| {
        let (a, d, i, o) = (w.alice.clone(), dex.clone(), wnear.id().clone(), usdc.id().clone());
        async move { a.call(d.id(), "set_rate").args_json(json!({"token_in": i, "token_out": o, "rate": r.to_string()})).transact().await?.into_result()?; anyhow::Ok(()) }
    };
    // Floor per NEAR = $3.75297364 / $0.99981068 x 0.99 = 3.7161474... USDC (Python fractions, x 1e24, floored):
    // 3716147444634218150180192. A pool paying 3.60 is refused, nothing sold.
    rate(36 * E24 / 10).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    let logs = r.logs().join("\n");
    assert!(logs.contains(r#""min_out_per_in":"3716147444634218150180192""#) && logs.contains("step_failed"), "{logs} {:?}", r.failures());
    // A pool at the market (3.75): sold, 37.5 USDC.
    rate(375 * E24 / 100).await?;
    let r = x(&w.keeper, &w.vault, "step", json!({"flow_id": f, "node": 0})).await?;
    println!("GAS relay step with a board-priced market floor: {:.1} Tgas", tgas(&r));
    assert!(r.logs().join("").contains(r#""out":"37500000000000000000""#), "{:?} {:?}", r.logs(), r.failures());

    // ---- the NVDA ladder: a rung at $185 fires on the board's $189.56, the $360 one waits ----
    x(&w.alice, &w.vault, "evm_setup", json!({"signer": w.signer.id(), "chain_id": 4663u64, "max_fee": GWEI.to_string(), "max_tip": (GWEI / 10).to_string(), "gas_cap": 1_500_000u64}))
        .await?.into_result()?;
    for t in common::templates("robinhood") {
        x(&w.alice, &w.vault, "evm_template", json!({"template": t})).await?.into_result()?;
    }
    let mut nodes = nvda_ladder();
    nodes[1]["when"]["price"] = json!((185 * E24).to_string());
    let lf: u32 = x(&w.alice, &w.vault, "save_flow", json!({"name": "NVDA ladder", "nodes": nodes})).await?.json()?;
    let eth_in = 30_000_000_000_000_000u128;
    x(&w.alice, &w.vault, "fund", json!({"flow_id": lf, "node": 0, "amount": eth_in.to_string()})).await?.into_result()?;
    let step = |node: u16, args: Value| x(&w.keeper, &w.vault, "step_remote", json!({"flow_id": lf, "node": node, "args": args}));
    step(0, evm_args(v4_values(1, true), 0)).await?.into_result()?;
    let got = 355_200_000_000_000_000u128;
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": lf, "node": 0, "out": got.to_string()})).await?.into_result()?;
    post_chainlink(&w.worker, &anyone, &evm).await.ok(); // keep it fresh (refused if the same second: not newer)
    let r = step(1, evm_args(v4_values(1, false), 1)).await?;
    println!("GAS relay step_remote (rung on a board price): {:.1} Tgas", tgas(&r));
    assert_eq!(event(&r, "remote_price")["price"], json!((18_955_682_289u128 * 10u128.pow(16)).to_string()), "$189.55682289 x 1e24");
    x(&w.keeper, &w.vault, "remote_report", json!({"flow_id": lf, "node": 1, "out": "9700000000000000"})).await?.into_result()?;
    assert!(fails_with(&step(2, evm_args(v4_values(1, false), 2)).await?, "NotYet"), "$360 not reached");

    // No fresh board price, no step: a minute on, the board's price is too old for the vault.
    let t = now(&w.worker).await?;
    while now(&w.worker).await? <= t + 61 {
        w.worker.fast_forward(50).await?;
    }
    let r = step(3, evm_args(v4_values(1, false), 2)).await?;
    assert!(fails_with(&r, "no price fresher than 60 s"), "{:?}", r.failures());
    let l = w.brain.view("get_flow").args_json(json!({"vault": w.vault, "flow_id": lf})).await?.json::<Value>()?["ledger"].clone();
    assert_eq!(inbox(&l, 3), got * 3_333 / 10_000, "nothing taken");
    Ok(())
}
