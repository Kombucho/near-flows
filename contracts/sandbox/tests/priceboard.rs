//! Memecoin prices on a price board (contracts/priceboard), behind Pyth's own
//! read interface. A memecoin has no trusted price, so no market floor reads one:
//! the vault names such a feed single_source and refuses a market floor on it;
//! the owner's fixed floor bounds the trade.
//! - Attested board: k of n attesters sign each price (Ed25519), the median is
//!   kept. Trusts the attesters.
//! - Wormhole board: a Wormhole Query response, signed by a quorum of guardians,
//!   carries a pump.fun bonding curve account (here the real bytes of a live
//!   curve, read from Solana mainnet; the guardians are test keys, since a real
//!   signed response needs a Wormhole Query API key: the real-signature checks
//!   are in priceboard's unit tests).

mod board;
mod common;
mod relayw;

use board::*;
use common::wasm;
use k256::ecdsa::SigningKey as Secp;
use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract, Worker};
use serde_json::{json, Value};

const ONE: u128 = 1_000_000_000_000_000_000;
const NEAR_ID: &str = "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750";
const SOL_ID: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

/// Fast-forward until the chain's clock passes `t` (sandbox blocks are under a second apart).
async fn wait_until(w: &Worker<Sandbox>, t: i64) -> anyhow::Result<()> {
    while now(w).await? <= t {
        w.fast_forward(50).await?;
    }
    Ok(())
}

async fn price(board: &Contract, method: &str, age: u64) -> anyhow::Result<Option<Value>> {
    Ok(board.view(method).args_json(json!({"price_id": MEME, "age": age})).await?.json()?)
}

#[tokio::test]
async fn attested_prices_need_k_fresh_signatures_from_real_attesters() -> anyhow::Result<()> {
    let w = near_workspaces::sandbox().await?;
    let keeper = w.dev_create_account().await?;
    let board = attested_board(&w, &keeper, 2).await?;
    let t = now(&w).await?;

    // The board signs-checks exactly the bytes built here.
    let bytes: String = board.view("attestation").args_json(json!({"feed": MEME, "price": "51204", "expo": -12, "publish_time": t})).await?.json()?;
    assert_eq!(bytes, hex::encode(message(MEME, 51_204, -12, t)));

    let (a, b, c) = (attester(0), attester(1), attester(2));
    let stranger = attester(9);
    let refused = |r: ExecutionFinalResult, why: &str| {
        let f = format!("{:?}", r.failures());
        assert!(f.contains(why), "expected '{why}': {f}");
    };
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t)]).await?, "1 of 3 attesters signed, 2 needed");
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t), report(&stranger, MEME, 51_204, t)]).await?, "is not an attester");
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t), report(&a, MEME, 51_204, t)]).await?, "one attester, one vote");
    let mut forged = report(&b, MEME, 51_204, t);
    forged["price"] = json!("99999"); // b signed 51204, not this
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t), forged]).await?, "bad attester signature");
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t + 120), report(&b, MEME, 51_204, t + 120)]).await?, "price from the future");
    refused(push(&keeper, &board, vec![report(&a, MEME, 51_204, t - 100), report(&b, MEME, 51_204, t)]).await?, "too far apart in time");
    assert!(price(&board, "get_price_no_older_than", 60).await?.is_none(), "nothing accepted so far");

    // Three votes, one an outlier: the median (51,300) stands, the spread is the uncertainty, the oldest time counts.
    let r = push(&keeper, &board, vec![report(&a, MEME, 51_204, t - 65), report(&b, MEME, 51_300, t - 60), report(&c, MEME, 90_000, t - 60)]).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let p = price(&board, "get_price_no_older_than", 300).await?.unwrap();
    assert_eq!((p["price"].as_str(), p["conf"].as_str(), p["expo"].as_i64(), p["publish_time"].as_i64()), (Some("51300"), Some("19398"), Some(-12), Some(t - 65)));
    // An old signed price replayed can't roll the board back.
    refused(push(&keeper, &board, vec![report(&a, MEME, 40_000, t - 90), report(&b, MEME, 40_000, t - 90)]).await?, "not newer");

    // A new price 60 s later replaces it: the board keeps the latest, nothing else.
    let r = push(&keeper, &board, vec![report(&a, MEME, 61_300, t - 5), report(&b, MEME, 61_300, t - 5)]).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(price(&board, "get_price_no_older_than", 60).await?.unwrap()["price"], json!("61300"));

    // Stale: once it's older than the reader's limit, there is no price.
    wait_until(&w, t + 30).await?;
    assert!(price(&board, "get_price_no_older_than", 30).await?.is_none());

    // Only the owner changes attesters.
    let r = keeper.call(board.id(), "set_attesters").args_json(json!({"attesters": [near_key(&a)], "k": 1, "max_skew_s": 60})).transact().await?;
    assert!(r.is_success());
    let other = w.dev_create_account().await?;
    refused(other.call(board.id(), "set_attesters").args_json(json!({"attesters": [near_key(&b)], "k": 1, "max_skew_s": 60})).transact().await?, "owner only");
    Ok(())
}

// ---------- the vault reads the board ----------

async fn token(w: &Worker<Sandbox>, owner: &Account, sym: &str, decimals: u8) -> anyhow::Result<Contract> {
    let t = w.dev_deploy(&wasm("mock_ft")).await?;
    t.call("new").args_json(json!({"owner_id": owner.id(), "total_supply": (u128::MAX / 4).to_string(), "name": sym, "symbol": sym, "decimals": decimals, "call_fee_bps": 0}))
        .transact().await?.into_result()?;
    Ok(t)
}

#[tokio::test]
async fn a_memecoin_on_the_board_gets_no_market_floor_only_the_owners_fixed_floor() -> anyhow::Result<()> {
    // A relay vault: the brain reads the board through Pyth's interface.
    let relayw::RelayWorld { worker: w, alice, keeper, vault, .. } = relayw::relay_world().await?;
    let root = w.dev_create_account().await?;
    let (wnear, meme) = (token(&w, &root, "wNEAR", 18).await?, token(&w, &root, "MEME", 6).await?);
    let dex = w.dev_deploy(&wasm("mock_dex")).await?;
    dex.call("new").args_json(json!({"lst": wnear.id()})).transact().await?.into_result()?;
    let pyth = w.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let board = attested_board(&w, &keeper, 2).await?;
    for t in [&wnear, &meme] {
        for who in [dex.id(), &vault] {
            root.call(t.id(), "storage_deposit").args_json(json!({"account_id": who})).deposit(NearToken::from_millinear(10)).transact().await?.into_result()?;
        }
    }
    root.call(meme.id(), "ft_transfer").args_json(json!({"receiver_id": dex.id(), "amount": (u128::MAX / 8).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    root.call(wnear.id(), "ft_transfer").args_json(json!({"receiver_id": vault, "amount": (1_000 * ONE).to_string()})).deposit(NearToken::from_yoctonear(1)).transact().await?.into_result()?;
    relayw::x(&alice, &vault, "trust", json!({"add": [{"contract": wnear.id()}, {"contract": meme.id()}, {"contract": dex.id()}], "remove": []})).await?.into_result()?;
    // NEAR from Pyth, confirmed by a second publisher; the memecoin from the board, single-source
    // (said plainly: the board's price is whatever the attesters, or the curve at the slot read, say).
    let r = relayw::x(&alice, &vault, "oracle_setup", json!({"config": {"max_age_s": 60, "max_conf_bps": 100, "tolerance_bps": 500, "feeds": [
        {"name": "near", "contract": pyth.id(), "price_id": NEAR_ID, "backup": common::backup(pyth.id(), NEAR_ID)},
        {"name": "meme", "contract": board.id(), "price_id": MEME, "backup": common::single_source()}]}})).await?.into_result()?;
    assert!(r.logs().iter().any(|l| l.contains(r#""feed":"meme""#) && l.contains("single-source feed: fixed floor + size caps apply")), "{:?}", r.logs());
    alice.call(pyth.id(), "set_price").args_json(json!({"price_id": NEAR_ID, "price": "200000000", "conf": "100000", "expo": -8, "backup": "200000000", "age_s": 0}))
        .transact().await?.into_result()?;
    // The attesters say $5.1204e-8 (the live curve's price at SOL $89.92): one post, right before the step.
    let t = now(&w).await?;
    push(&keeper, &board, vec![report(&attester(0), MEME, 51_204, t), report(&attester(1), MEME, 51_204, t)]).await?.into_result()?;

    // Sell 100 wNEAR for MEME at most 1% under the market: refused, a market floor can't read a single-source feed.
    let swap = |market: Value, per_in: Value| json!({"input": wnear.id(), "when": {"when": "now"}, "action": {"label": "Buy MEME", "out": meme.id(), "min_out_per_in": per_in, "price": null,
        "calls": [{"call": "ft", "receiver": dex.id(), "tgas": 50, "msg": format!(r#"{{"token_out":"{}","min_out":"{{min_out}}"}}"#, meme.id())}],
        "market": market}});
    let flow = |node: Value| {
        let (alice, vault) = (alice.clone(), vault.clone());
        async move {
            let f: u32 = relayw::x(&alice, &vault, "save_flow", json!({"name": "ape", "nodes": [node]})).await?.into_result()?.json()?;
            relayw::x(&alice, &vault, "fund", json!({"flow_id": f, "node": 0, "amount": (100 * ONE).to_string()})).await?.into_result()?;
            anyhow::Ok(f)
        }
    };
    let rate = |r: u128| dex.call("set_rate").args_json(json!({"token_in": wnear.id(), "token_out": meme.id(), "rate": r.to_string()})).transact();
    let step = |f: u32| relayw::x(&keeper, &vault, "step", json!({"flow_id": f, "node": 0}));
    rate(39_059_448_480_587_454_105).await?.into_result()?; // the market (Python fractions: 2e18 / 5.1204e10 / 1e12 per base unit)
    let market = flow(swap(json!({"sell": "near", "buy": "meme", "dec_in": 18, "dec_out": 6, "slippage_bps": 100}), Value::Null)).await?;
    let r = step(market).await?;
    assert!(format!("{:?}", r.failures()).contains("meme: single-source feed: fixed floor + size caps apply (no market floor)"), "{:?} {:?}", r.logs(), r.failures());
    assert!(!r.logs().join("\n").contains(r#""event":"step""#), "it didn't run");

    // The owner's fixed floor instead: $2 / $5.1204e-8 x 0.99 per wNEAR (scaled 1e24 per base unit:
    // 38668853995781579564, Python fractions). A pool pushed 5% under the market is refused.
    let fixed = flow(swap(Value::Null, json!("38668853995781579564"))).await?;
    rate(37_106_476_056_558_081_399).await?.into_result()?;
    let r = step(fixed).await?;
    assert!(r.logs().join("\n").contains("step_failed"), "{:?} {:?}", r.logs(), r.failures());
    // Back at the market: it buys ~3.906e15 units (3.906 billion MEME) for 100 wNEAR.
    rate(39_059_448_480_587_454_105).await?.into_result()?;
    let r = step(fixed).await?;
    assert!(r.logs().join("\n").contains(r#""out":"3905944848058745""#), "{:?} {:?}", r.logs(), r.failures());
    Ok(())
}

// ---------- Wormhole board ----------

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

fn curve() -> (Vec<u8>, Vec<u8>) {
    let c: Value = serde_json::from_str(&std::fs::read_to_string(format!("{}/../priceboard/tests-data/pump_curve.json", env!("CARGO_MANIFEST_DIR"))).unwrap()).unwrap();
    (hex::decode(c["owner_hex"].as_str().unwrap()).unwrap(), hex::decode(c["data_hex"].as_str().unwrap()).unwrap())
}

/// A sol_pda query response (proxy format) for MEME's bonding curve.
fn response(commitment: &str, owner: &[u8], data: &[u8], block_time_s: i64) -> Vec<u8> {
    let pump = curve().0; // the pump program id
    let mut q = Vec::new();
    var(&mut q, commitment.as_bytes());
    q.extend([0u8; 24]);
    q.push(1);
    q.extend(&pump);
    q.push(2);
    var(&mut q, b"bonding-curve");
    var(&mut q, &hex::decode(MEME).unwrap());
    let mut a = Vec::new();
    a.extend(466_074_933u64.to_be_bytes());
    a.extend(((block_time_s as u64) * 1_000_000).to_be_bytes());
    a.extend([7u8; 32]);
    a.push(1);
    a.extend([9u8; 32]);
    a.push(249);
    a.extend(1_493_521u64.to_be_bytes());
    a.extend(u64::MAX.to_be_bytes());
    a.push(0);
    a.extend(owner);
    var(&mut a, data);
    let mut req = vec![1u8];
    req.extend(7u32.to_be_bytes());
    req.extend([1, 0, 1, 5]);
    var(&mut req, &q);
    let mut out = vec![1u8, 0, 0];
    out.extend([0u8; 65]);
    var(&mut out, &req);
    out.extend([1, 0, 1, 5]);
    var(&mut out, &a);
    out
}

/// keccak256(prefix || keccak256(response)), signed by these test keys sitting
/// at these guardian indices (proxy format: r||s||v||index, hex).
fn sign_as(resp: &[u8], keys: &[u8], at: &[u8]) -> Vec<String> {
    let mut m = b"query_response_0000000000000000000|".to_vec();
    m.extend(evmtx::keccak256(resp));
    let d = evmtx::keccak256(&m);
    keys.iter().zip(at).map(|(&k, &i)| {
        let (s, rec) = guardian(k).sign_prehash_recoverable(&d).unwrap();
        let mut b = s.to_bytes().to_vec();
        b.extend([rec.to_byte(), i]);
        hex::encode(b)
    }).collect()
}

fn sign(resp: &[u8], who: &[u8]) -> Vec<String> {
    sign_as(resp, who, who)
}

#[tokio::test]
async fn a_wormhole_board_prices_a_pump_curve_from_a_guardian_signed_query() -> anyhow::Result<()> {
    let w = near_workspaces::sandbox().await?;
    let keeper = w.dev_create_account().await?;
    let pyth = w.dev_deploy(&wasm("mock_pyth")).await?;
    pyth.call("new").transact().await?.into_result()?;
    let board = w.dev_deploy(&wasm("priceboard")).await?;
    board.call("new").args_json(json!({"owner": null, "source": {"kind": "wormhole", "guardian_set": 0,
        "guardians": (0..3).map(|i| guardian_addr(&guardian(i))).collect::<Vec<_>>(), "pyth": pyth.id(), "quote_age_s": 60,
        "quotes": [{"mint": "0".repeat(64), "pyth_id": SOL_ID, "decimals": 9}]}})).transact().await?.into_result()?;
    let sol = |age: i64| keeper.call(pyth.id(), "set_price").args_json(json!({"price_id": SOL_ID, "price": "8991598560", "conf": "4495799", "expo": -8, "age_s": age})).transact();
    let t = now(&w).await?;
    sol(0).await?.into_result()?;
    let (pump, data) = curve();
    let post = |resp: Vec<u8>, sigs: Vec<String>, deposit: u128| keeper.call(board.id(), "push_query")
        .args_json(json!({"response": hex::encode(resp), "signatures": sigs})).deposit(NearToken::from_yoctonear(deposit)).max_gas().transact();
    let refused = |r: ExecutionFinalResult, why: &str| {
        let f = format!("{:?}", r.failures());
        assert!(f.contains(why), "expected '{why}': {f}");
    };
    let fee = NearToken::from_millinear(5).as_yoctonear();
    let good = response("finalized", &pump, &data, t);

    refused(post(good.clone(), sign(&good, &[0, 1]), fee).await?, "no quorum"); // 2 of 3
    refused(post(good.clone(), sign(&good, &[0, 1, 1]), fee).await?, "guardian indices must increase");
    let mut other = sign(&good, &[0, 1, 2]);
    other[2] = sign(&response("finalized", &pump, &data, t - 1), &[2])[0].clone(); // guardian 2 signed something else
    refused(post(good.clone(), other, fee).await?, "bad guardian signature");
    let r = response("confirmed", &pump, &data, t);
    refused(post(r.clone(), sign(&r, &[0, 1, 2]), fee).await?, "finalized commitment only");
    let r = response("finalized", &[3u8; 32], &data, t); // look-alike data owned by another program
    refused(post(r.clone(), sign(&r, &[0, 1, 2]), fee).await?, "not a pump.fun account");
    let mut done = data.clone();
    done[48] = 1;
    let r = response("finalized", &pump, &done, t);
    refused(post(r.clone(), sign(&r, &[0, 1, 2]), fee).await?, "curve complete");
    refused(post(good.clone(), sign(&good, &[0, 1, 2]), 0).await?, "per new coin");

    // Signed by all three: $5.1204e-8 (0.614682791 SOL / 1.0794e9 coins x $89.9159856), SOL's 0.05% uncertainty carried over.
    let r = post(good.clone(), sign(&good, &[0, 1, 2]), fee).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    let p = price(&board, "get_price_no_older_than", 60).await?.unwrap();
    assert_eq!((p["price"].as_str(), p["conf"].as_str(), p["expo"].as_i64(), p["publish_time"].as_i64()), (Some("51204"), Some("25"), Some(-12), Some(t)));

    // A newer curve reading but SOL's price is stale: nothing written.
    sol(120).await?.into_result()?;
    let newer = response("finalized", &pump, &data, t + 1);
    let r = post(newer.clone(), sign(&newer, &[0, 1, 2]), 0).await?;
    assert!(format!("{:?}", r.failures()).contains("no fresh Pyth price"), "{:?}", r.failures());
    let last: Value = board.view("get_price_unsafe").args_json(json!({"price_id": MEME})).await?.json::<Option<Value>>()?.unwrap();
    assert_eq!(last["publish_time"].as_i64(), Some(t), "still the earlier reading");

    // Wormhole rotates its guardians: the set signs its successor (a governance VAA), anyone posts it.
    let next: Vec<String> = (3..6).map(|i| guardian_addr(&guardian(i))).collect();
    let mut body = vec![0u8; 8];
    body.extend(1u16.to_be_bytes());
    body.extend([0u8; 31]);
    body.push(4);
    body.extend([0u8; 9]);
    body.extend([0u8; 28]);
    body.extend(b"Core");
    body.push(2);
    body.extend(0u16.to_be_bytes());
    body.extend(1u32.to_be_bytes());
    body.push(3);
    for g in &next {
        body.extend(hex::decode(g)?);
    }
    let d = evmtx::keccak256(&evmtx::keccak256(&body));
    let mut vaa = vec![1u8, 0, 0, 0, 0, 3];
    for i in 0..3u8 {
        let (s, rec) = guardian(i).sign_prehash_recoverable(&d).unwrap();
        vaa.push(i);
        vaa.extend(s.to_bytes());
        vaa.push(rec.to_byte());
    }
    vaa.extend(&body);
    let r = keeper.call(board.id(), "upgrade_guardians").args_json(json!({"vaa": hex::encode(&vaa)})).max_gas().transact().await?;
    assert_eq!(r.into_result()?.json::<u32>()?, 1);
    sol(0).await?.into_result()?;
    let later = response("finalized", &pump, &data, t + 2);
    refused(post(later.clone(), sign(&later, &[0, 1, 2]), 0).await?, "bad guardian signature"); // the old set no longer counts
    let r = post(later.clone(), sign_as(&later, &[3, 4, 5], &[0, 1, 2]), 0).await?;
    assert!(r.is_success(), "{:?}", r.failures());
    assert_eq!(price(&board, "get_price_no_older_than", 60).await?.unwrap()["publish_time"].as_i64(), Some(t + 2));
    Ok(())
}
