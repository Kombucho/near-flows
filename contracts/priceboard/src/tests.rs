//! Real vectors where they exist (tests-data/, all public mainnet data read
//! 2026-11-06): two query responses the mainnet guardians signed (set 4), the
//! VAAs in which set 4 approved 5, 5 approved 6 and 6 approved 7, set 7 as
//! Ethereum's core contract reports it today, and a live pump.fun curve account.
//! A signed sol_pda response needs a Wormhole Query API key (none here), so that
//! one is built from the real curve bytes and signed by test guardian keys.
//! Likewise for Chainlink: chainlink_reads.json holds real latestRoundData()
//! and decimals() results (NVDA on Robinhood Chain, NEAR on Arbitrum) read by
//! eth_call at a pinned block; the eth_call response around them is built here
//! and signed by test guardians (a real signed one needs WORMHOLE_QUERY_API_KEY).

use super::*;
use crate::wormhole::*;
use k256::ecdsa::SigningKey;

fn data(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests-data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn set(name: &str) -> Vec<[u8; 20]> {
    data(name).lines().map(|l| hex::decode(l).unwrap().try_into().unwrap()).collect()
}

fn sigs(hexes: &[serde_json::Value]) -> Vec<(u8, [u8; 65])> {
    hexes.iter().map(|s| proxy_sig(s.as_str().unwrap()).unwrap()).collect()
}

#[test]
fn real_mainnet_query_responses_verify_against_guardian_set_4() {
    let q: serde_json::Value = serde_json::from_str(&data("mainnet_queries.json")).unwrap();
    let gs4 = set("guardian_set_4.txt");
    assert_eq!(quorum(gs4.len()), 13);
    for (name, chain) in [("wethNameResponse", 2u16), ("opWethNameResponse", 24)] {
        let bytes = hex::decode(q[name]["bytes"].as_str().unwrap()).unwrap();
        let s = sigs(q[name]["signatures"].as_array().unwrap());
        assert_eq!(s.len(), 13, "{name}: exactly a quorum");
        verify_quorum(&response_digest(&bytes), &s, &gs4).unwrap();
        // eth_call (type 1) of WETH's name() on Ethereum (2) and Optimism (24).
        let per = parse_response(&bytes).unwrap();
        assert_eq!((per.len(), per[0].chain, per[0].kind), (1, chain, 1));
        assert!(per[0].response.windows(13).any(|w| w == b"Wrapped Ether"), "{name}: the answer is in there");

        // One byte changed: the signatures no longer match.
        let mut bad = bytes.clone();
        *bad.last_mut().unwrap() ^= 1;
        assert_eq!(verify_quorum(&response_digest(&bad), &s, &gs4), Err("bad guardian signature"));
        // 12 of 19 is not a quorum; the same guardian twice doesn't count twice; order matters.
        assert_eq!(verify_quorum(&response_digest(&bytes), &s[1..], &gs4), Err("no quorum"));
        let mut dup = s.clone();
        dup[1] = dup[0];
        assert_eq!(verify_quorum(&response_digest(&bytes), &dup, &gs4), Err("guardian indices must increase"));
        // The right signatures against today's set 7: guardians 5..8 and 14, 15, 17, 18 changed since.
        assert!(verify_quorum(&response_digest(&bytes), &s, &set("guardian_set_7.txt")).is_err());
        // A truncated response doesn't parse.
        assert!(parse_response(&bytes[..bytes.len() - 1]).is_err());
    }
}

#[test]
fn real_guardian_upgrades_lead_from_set_4_to_todays_set_7() {
    let (mut index, mut current) = (4u32, set("guardian_set_4.txt"));
    let vaas: Vec<Vec<u8>> = data("guardian_set_vaas.csv").lines().map(|l| hex::decode(l.split(',').nth(1).unwrap()).unwrap()).collect();
    // Set 6's VAA was signed by set 5: refused while the board is on set 4.
    assert_eq!(guardian_upgrade(&vaas[1], index, &current), Err("VAA not signed by the current guardian set"));
    for vaa in &vaas {
        let up = guardian_upgrade(vaa, index, &current).unwrap();
        assert_eq!((up.signed_by, up.index, up.guardians.len()), (index, index + 1, 19));
        (index, current) = (up.index, up.guardians);
    }
    assert_eq!(index, 7);
    assert_eq!(current, set("guardian_set_7.txt"), "the same set Ethereum's core contract reports today");
    // A tampered VAA (one new guardian's address changed) fails its signatures.
    let mut bad = vaas[0].clone();
    *bad.last_mut().unwrap() ^= 1;
    assert_eq!(guardian_upgrade(&bad, 4, &set("guardian_set_4.txt")), Err("bad guardian signature"));
}

// ---------- sol_pda responses (test guardians) ----------

pub struct Curve {
    pub mint: [u8; 32],
    pub owner: [u8; 32],
    pub data: Vec<u8>,
}

fn var(out: &mut Vec<u8>, b: &[u8]) {
    out.extend_from_slice(&(b.len() as u32).to_be_bytes());
    out.extend_from_slice(b);
}

/// A sol_pda query response for these curves, in the proxy's format (offline requester).
pub fn sol_pda_response(curves: &[Curve], commitment: &str, block_time_us: u64) -> Vec<u8> {
    let mut q = Vec::new();
    var(&mut q, commitment.as_bytes());
    q.extend_from_slice(&[0u8; 24]); // min slot, slice offset, slice length
    q.push(curves.len() as u8);
    let mut a = Vec::new();
    a.extend_from_slice(&466_074_933u64.to_be_bytes());
    a.extend_from_slice(&block_time_us.to_be_bytes());
    a.extend_from_slice(&[7u8; 32]);
    a.push(curves.len() as u8);
    for c in curves {
        q.extend_from_slice(&pump::PROGRAM);
        q.push(2);
        var(&mut q, pump::SEED);
        var(&mut q, &c.mint);
        a.extend_from_slice(&[9u8; 32]); // the PDA (not checked: the guardians derived it)
        a.push(255);
        a.extend_from_slice(&1_493_521u64.to_be_bytes());
        a.extend_from_slice(&u64::MAX.to_be_bytes());
        a.push(0);
        a.extend_from_slice(&c.owner);
        var(&mut a, &c.data);
    }
    let mut req = vec![1u8];
    req.extend_from_slice(&42u32.to_be_bytes());
    req.push(1);
    req.extend_from_slice(&SOLANA.to_be_bytes());
    req.push(QT_SOL_PDA);
    var(&mut req, &q);
    let mut out = vec![1u8, 0, 0];
    out.extend_from_slice(&[0u8; 65]);
    var(&mut out, &req);
    out.push(1);
    out.extend_from_slice(&SOLANA.to_be_bytes());
    out.push(QT_SOL_PDA);
    var(&mut out, &a);
    out
}

pub fn guardian(i: u8) -> SigningKey {
    SigningKey::from_slice(&[i + 1; 32]).unwrap()
}

pub fn guardian_addr(k: &SigningKey) -> [u8; 20] {
    let pt = k.verifying_key().to_encoded_point(false);
    env::keccak256_array(&pt.as_bytes()[1..])[12..].try_into().unwrap()
}

/// Proxy-format signatures (r||s||v||index, hex) by these test guardians.
pub fn sign(response: &[u8], who: &[u8]) -> Vec<String> {
    let d = response_digest(response);
    who.iter()
        .map(|&i| {
            let (sig, rec) = guardian(i).sign_prehash_recoverable(&d).unwrap();
            let mut b = sig.to_bytes().to_vec();
            b.push(rec.to_byte());
            b.push(i);
            hex::encode(b)
        })
        .collect()
}

pub fn real_curve() -> Curve {
    let c: serde_json::Value = serde_json::from_str(&data("pump_curve.json")).unwrap();
    let h = |k: &str| hex::decode(c[k].as_str().unwrap()).unwrap();
    Curve { mint: h("mint_hex").try_into().unwrap(), owner: h("owner_hex").try_into().unwrap(), data: h("data_hex") }
}

#[test]
fn a_signed_sol_pda_response_yields_the_live_curves_price() {
    let set: Vec<[u8; 20]> = (0..3).map(|i| guardian_addr(&guardian(i))).collect();
    let curve = real_curve();
    assert_eq!(curve.owner, pump::PROGRAM, "the recorded account is the pump program's");
    let resp = sol_pda_response(&[curve], "finalized", 1_793_960_711_000_000);
    let s: Vec<_> = sign(&resp, &[0, 1, 2]).iter().map(|h| proxy_sig(h).unwrap()).collect();
    verify_quorum(&response_digest(&resp), &s, &set).unwrap();
    assert_eq!(verify_quorum(&response_digest(&resp), &s[..2], &set), Err("no quorum"), "3 guardians: all 3 needed");

    let q = parse_sol_pda(&parse_response(&resp).unwrap()[0]).unwrap();
    assert_eq!((q.commitment.as_slice(), q.slice, q.block_time_us / 1_000_000), (&b"finalized"[..], (0, 0), 1_793_960_711));
    let a = &q.accounts[0];
    assert_eq!((a.program, a.seeds[0].as_slice(), a.owner), (pump::PROGRAM, pump::SEED, pump::PROGRAM));
    let c = pump::decode(&a.data).unwrap();
    assert_eq!((c.virtual_token, c.virtual_quote, c.complete, c.quote_mint), (1_079_397_334_145_264, 614_682_791, false, [0; 32]));
    // At SOL = $89.91598560 (Pyth: 8991598560 x 1e-8): 0.614682791 SOL / 1,079,397,334.145264 coins x $89.9159856
    // = $5.12043e-8 per coin; Python fractions: floor(x 1e12) = 51204. Jupiter said $5.467e-8 about 30 min earlier.
    assert_eq!(pump::usd_e12(&c, 8_991_598_560, -8, 9), Some(51_204));
    // Were it USDC-quoted (6 decimals) at $0.9998: 569354 (Python fractions).
    assert_eq!(pump::usd_e12(&c, 99_980_000, -8, 6), Some(569_354));
    // Limits: an empty curve or no quote price is no price.
    assert_eq!(pump::usd_e12(&pump::Curve { virtual_token: 0, ..c.clone() }, 1, -8, 9), None);
    assert_eq!(pump::usd_e12(&c, 0, -8, 9), None);
    assert!(pump::decode(&a.data[..48]).is_err() && pump::decode(&[0u8; 166]).is_err());
}

// ---------- Chainlink feeds via eth_call (Evm board) ----------

#[test]
fn real_eth_call_responses_parse_as_calls_with_their_block() {
    // The same real responses as above (guardian set 4): WETH's name() on Ethereum and Optimism.
    let q: serde_json::Value = serde_json::from_str(&data("mainnet_queries.json")).unwrap();
    let bytes = hex::decode(q["wethNameResponse"]["bytes"].as_str().unwrap()).unwrap();
    let c = parse_eth_call(&parse_response(&bytes).unwrap()[0]).unwrap();
    assert_eq!((c.chain, c.block_id.as_slice(), c.block_num), (2, &b"0x124c0d6"[..], 0x124c0d6));
    assert_eq!(c.block_time_us / 1_000_000, 1_707_424_775, "Feb 2024, when the example was recorded");
    assert_eq!(c.calls.len(), 1);
    assert_eq!((hex::encode(c.calls[0].to), hex::encode(&c.calls[0].data)), ("c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2".into(), "06fdde03".into()));
    assert_eq!(&c.calls[0].result[64..77], b"Wrapped Ether");
    let op = hex::decode(q["opWethNameResponse"]["bytes"].as_str().unwrap()).unwrap();
    let c = parse_eth_call(&parse_response(&op).unwrap()[0]).unwrap();
    assert_eq!((c.chain, hex::encode(c.calls[0].to)), (24, "4200000000000000000000000000000000000006".into()));
    // A Solana query is not an eth_call.
    let sol = sol_pda_response(&[real_curve()], "finalized", 1);
    assert_eq!(parse_eth_call(&parse_response(&sol).unwrap()[0]), Err("not an EVM eth_call query"));
}

/// A real Chainlink read recorded in tests-data/chainlink_reads.json.
pub struct Read {
    pub chain: u16,
    pub address: [u8; 20],
    pub block: u64,
    pub hash: [u8; 32],
    pub time: u64,
    pub round: Vec<u8>,
    pub decimals: Vec<u8>,
}

pub fn read(name: &str) -> Read {
    let j: serde_json::Value = serde_json::from_str(&data("chainlink_reads.json")).unwrap();
    let f = &j["feeds"][name];
    let h = |k: &str| hex::decode(f[k].as_str().unwrap().trim_start_matches("0x")).unwrap();
    Read {
        chain: f["wormhole_chain"].as_u64().unwrap() as u16,
        address: h("address").try_into().unwrap(),
        block: f["block_number"].as_u64().unwrap(),
        hash: h("block_hash").try_into().unwrap(),
        time: f["block_time"].as_u64().unwrap(),
        round: h("latestRoundData"),
        decimals: h("decimals"),
    }
}

#[test]
fn real_chainlink_answers_decode_to_nvda_and_near_prices() {
    // NVDA on Robinhood Chain (RHNVDA / USD: the token's price, stock x uiMultiplier 1.000775).
    let n = read("nvda_robinhood");
    let r = chainlink::decode_round(&n.round).unwrap();
    assert_eq!((r.answer, r.updated_at, chainlink::decode_decimals(&n.decimals)), (18_955_682_289, 1_793_979_985, Ok(8)));
    assert_eq!(r.round_id, (1u128 << 64) | 1584, "phase 1, round 1584");
    assert_eq!(n.time - r.updated_at, 528, "updated 528 s before the block (it moves on 0.5% deviation)");
    // NEAR on Arbitrum: $3.75297364 (CoinGecko said $3.75 at the time).
    let a = read("near_arbitrum");
    let r = chainlink::decode_round(&a.round).unwrap();
    assert_eq!((r.answer, r.updated_at, chainlink::decode_decimals(&a.decimals)), (375_297_364, 1_793_980_371, Ok(8)));

    // Limits: a negative or zero answer, one beyond i64, a short result, too many decimals.
    let mut neg = n.round.clone();
    neg[32..64].copy_from_slice(&[0xff; 32]); // int256 -1
    assert_eq!(chainlink::decode_round(&neg), Err("answer is negative or too large"));
    let mut zero = n.round.clone();
    zero[32..64].copy_from_slice(&[0; 32]);
    assert_eq!(chainlink::decode_round(&zero), Err("answer is zero"));
    let mut big = n.round.clone();
    big[56] = 0x80; // 2^63: one past i64::MAX
    assert_eq!(chainlink::decode_round(&big), Err("answer is negative or too large"));
    assert!(chainlink::decode_round(&n.round[..128]).is_err());
    let mut d = n.decimals.clone();
    d[31] = 19;
    assert!(chainlink::decode_decimals(&d).is_err());
    // The id says where the price comes from: chain 72, zeros, the proxy.
    assert_eq!(hex::encode(chainlink::price_id(72, &n.address)), "004800000000000000000000379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15");
}

/// A query response (proxy format, offline requester): one eth_call per read,
/// each with these (call data, result) pairs, at the read's recorded block.
pub fn eth_call_response(reads: &[(&Read, Vec<(Vec<u8>, Vec<u8>)>)]) -> Vec<u8> {
    let mut req = vec![1u8];
    req.extend_from_slice(&7u32.to_be_bytes());
    req.push(reads.len() as u8);
    let mut resp = vec![reads.len() as u8];
    for (r, calls) in reads {
        let mut q = Vec::new();
        var(&mut q, format!("0x{:x}", r.block).as_bytes());
        q.push(calls.len() as u8);
        let mut a = Vec::new();
        a.extend_from_slice(&r.block.to_be_bytes());
        a.extend_from_slice(&r.hash);
        a.extend_from_slice(&(r.time * 1_000_000).to_be_bytes());
        a.push(calls.len() as u8);
        for (data, result) in calls {
            q.extend_from_slice(&r.address);
            var(&mut q, data);
            var(&mut a, result);
        }
        req.extend_from_slice(&r.chain.to_be_bytes());
        req.push(QT_ETH_CALL);
        var(&mut req, &q);
        resp.extend_from_slice(&r.chain.to_be_bytes());
        resp.push(QT_ETH_CALL);
        var(&mut resp, &a);
    }
    let mut out = vec![1u8, 0, 0];
    out.extend_from_slice(&[0u8; 65]);
    var(&mut out, &req);
    out.extend_from_slice(&resp);
    out
}

fn feed_calls(r: &Read) -> Vec<(Vec<u8>, Vec<u8>)> {
    vec![(chainlink::LATEST_ROUND_DATA.to_vec(), r.round.clone()), (chainlink::DECIMALS.to_vec(), r.decimals.clone())]
}

fn evm_feed(r: &Read, heartbeat_s: u64) -> EvmFeed {
    EvmFeed { chain: r.chain, address: format!("0x{}", hex::encode(r.address)), deviation_bps: 50, heartbeat_s }
}

fn clock_as(t: u64, who: &str) {
    near_sdk::testing_env!(near_sdk::test_utils::VMContextBuilder::new().block_timestamp(t * 1_000_000_000).predecessor_account_id(who.parse().unwrap()).build());
}

fn clock(t: u64) {
    clock_as(t, "anyone.near");
}

/// An Evm board of 3 test guardians with the NVDA and NEAR feeds, at unix time `t`.
fn evm_board(t: u64, nvda_heartbeat: u64) -> Board {
    clock(t);
    let guardians = (0..3).map(|i| hex::encode(guardian_addr(&guardian(i)))).collect();
    let feeds = vec![evm_feed(&read("nvda_robinhood"), nvda_heartbeat), evm_feed(&read("near_arbitrum"), 86_400)];
    Board::new(Some("owner.near".parse().unwrap()), Source::Evm { guardian_set: 0, guardians, feeds, max_block_age_s: 60 })
}

/// The panic message of `f`, or None if it didn't panic.
fn refusal(f: impl FnOnce()) -> Option<String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::panic::set_hook(hook);
    r.err().map(|e| e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default())
}

#[test]
fn a_guardian_signed_chainlink_read_becomes_a_pyth_format_price() {
    let (n, a) = (read("nvda_robinhood"), read("near_arbitrum"));
    let t = a.time + 10;
    let mut b = evm_board(t, 86_400);
    // Both feeds in one response: NVDA on Robinhood Chain and NEAR on Arbitrum, each at its own block.
    let resp = eth_call_response(&[(&n, feed_calls(&n)), (&a, feed_calls(&a))]);
    let out = b.push_evm(hex::encode(&resp), sign(&resp, &[0, 1, 2]));
    assert_eq!(out.len(), 2);
    let nvda = b.evm_price_id(72, "0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15".into());
    let near = b.evm_price_id(23, "0xbf5c3fb2633e924598a46b9d07a174a9dbcf57c0".into());
    // $189.55682289, +-0.5% (the feed's deviation threshold), published at the block's time.
    assert_eq!(b.get_price_no_older_than(nvda, 60), Some(Price { price: I64(18_955_682_289), conf: U64(94_778_411), expo: -8, publish_time: n.time as i64 }));
    assert_eq!(b.get_price_no_older_than(near.clone(), 60), Some(Price { price: I64(375_297_364), conf: U64(1_876_486), expo: -8, publish_time: a.time as i64 }));
    // The same response again: nothing newer.
    assert!(refusal(|| { b.push_evm(hex::encode(&resp), sign(&resp, &[0, 1, 2])); }).unwrap().contains("not newer"));
    // A minute later the reader's 60 s limit says: no price.
    clock(t + 60);
    assert!(b.get_price_no_older_than(near, 60).is_none());
}

#[test]
fn an_evm_board_refuses_what_the_guardians_did_not_vouch_for() {
    let n = read("nvda_robinhood");
    let t = n.time + 5;
    let post = |b: &mut Board, resp: &[u8], who: &[u8]| refusal(|| { b.push_evm(hex::encode(resp), sign(resp, who)); }).unwrap_or_default();
    let good = eth_call_response(&[(&n, feed_calls(&n))]);
    let mut b = evm_board(t, 86_400);

    // 2 of 3 guardians; one result byte changed after signing.
    assert!(post(&mut b, &good, &[0, 1]).contains("no quorum"));
    let sigs = sign(&good, &[0, 1, 2]);
    let mut bad = good.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(refusal(|| { b.push_evm(hex::encode(&bad), sigs.clone()); }).unwrap().contains("bad guardian signature"));
    // The right address on the wrong chain; an address nobody registered.
    let wrong_chain = Read { chain: 23, ..read("nvda_robinhood") };
    assert!(post(&mut b, &eth_call_response(&[(&wrong_chain, feed_calls(&n))]), &[0, 1, 2]).contains("on chain 23 is not a registered feed"));
    let stranger = Read { address: [0x11; 20], ..read("nvda_robinhood") };
    assert!(post(&mut b, &eth_call_response(&[(&stranger, feed_calls(&n))]), &[0, 1, 2]).contains("is not a registered feed"));
    // Other call data: description() instead of latestRoundData(), or no decimals() after it.
    let mut other = feed_calls(&n);
    other[0].0 = vec![0x72, 0x84, 0xe4, 0x16];
    assert!(post(&mut b, &eth_call_response(&[(&n, other)]), &[0, 1, 2]).contains("latestRoundData() then decimals()"));
    assert!(post(&mut b, &eth_call_response(&[(&n, feed_calls(&n)[..1].to_vec())]), &[0, 1, 2]).contains("latestRoundData() then decimals()"));
    // A negative answer (int256 -1).
    let mut neg = feed_calls(&n);
    neg[0].1[32..64].copy_from_slice(&[0xff; 32]);
    assert!(post(&mut b, &eth_call_response(&[(&n, neg)]), &[0, 1, 2]).contains("answer is negative"));
    // The block: more than 60 s old when posted, or from the future.
    clock(n.time + 61);
    assert!(post(&mut b, &good, &[0, 1, 2]).contains("block 61 s old"));
    clock(n.time - 31);
    assert!(post(&mut b, &good, &[0, 1, 2]).contains("block from the future"));
    // The feed itself is stale: its answer is 528 s old at the block, past a 300 s heartbeat.
    let mut strict = evm_board(t, 300);
    assert!(post(&mut strict, &good, &[0, 1, 2]).contains("answer 528 s old at the block, heartbeat 300 s"));
    // An eth_call board takes neither pump.fun queries nor attestations.
    assert!(refusal(|| { let _ = b.push_query(hex::encode(&good), sign(&good, &[0, 1, 2])); }).unwrap().contains("doesn't take pump.fun"));
    assert!(refusal(|| { b.push("00".repeat(32), vec![]); }).unwrap().contains("not attestations"));
    // All of that right: taken.
    clock(t);
    assert_eq!(b.push_evm(hex::encode(&good), sign(&good, &[0, 1, 2]))[0].price, I64(18_955_682_289));
}

#[test]
fn only_the_owner_lists_feeds_and_bad_listings_are_refused() {
    let n = read("nvda_robinhood");
    let mut b = evm_board(n.time, 86_400);
    assert!(refusal(|| b.set_evm_feeds(vec![])).unwrap().contains("owner only"));
    clock_as(n.time, "owner.near");
    let f = evm_feed(&n, 86_400);
    assert!(refusal(|| b.set_evm_feeds(vec![f.clone(), f.clone()])).unwrap().contains("listed twice"));
    assert!(refusal(|| b.set_evm_feeds(vec![EvmFeed { address: format!("0x{}", hex::encode(n.address).to_uppercase()), ..f.clone() }])).unwrap().contains("lowercase"));
    assert!(refusal(|| b.set_evm_feeds(vec![EvmFeed { chain: 1, ..f.clone() }])).unwrap().contains("EVM chain"));
    assert!(refusal(|| b.set_evm_feeds(vec![EvmFeed { deviation_bps: 0, ..f.clone() }])).unwrap().contains("deviation_bps"));
    b.set_evm_feeds(vec![f]);
    assert!(matches!(b.get_source(), Source::Evm { feeds, .. } if feeds.len() == 1));
}

// ---------- a curve price is the spot at its slot ----------

const SOL_ID: &str = "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d";

/// A pump.fun board at unix time `t`, SOL-quoted curves.
fn pump_board(t: u64) -> Board {
    clock(t);
    let quotes = vec![Quote { mint: "00".repeat(32), pyth_id: SOL_ID.into(), decimals: 9 }];
    Board::new(None, Source::Wormhole { guardian_set: 0, guardians: vec!["11".repeat(20)], pyth: "pyth.near".parse().unwrap(), quotes, quote_age_s: 60 })
}

/// The board prices the live curve with `extra` lamports bought into it (constant product), read at `t`.
fn post_curve(b: &mut Board, t: u64, extra: u64) {
    let (vt, vq) = (1_079_397_334_145_264u128, 614_682_791u128);
    let q = vq + extra as u128;
    let pending = vec![Pending { feed: hex::encode(real_curve().mint), virtual_token: U64((vt * vq / q) as u64), virtual_quote: U64(q as u64), time: t as i64 }];
    clock_as(t, "alice.near"); // on_quote is the board's own callback
    let sol = Price { price: I64(8_991_598_560), conf: U64(4_495_799), expo: -8, publish_time: t as i64 };
    b.on_quote(pending, 9, Ok(Some(sol)));
    clock(t);
}

#[test]
fn a_curve_read_is_the_spot_at_the_slot_read_and_nothing_vouches_for_it() {
    // Anyone who buys into the curve and has the guardians read that slot moves
    // the board's price with it: the board keeps the latest read, no average.
    // That is why a pump.fun coin gets no market floor (a vault names such a feed
    // single_source; its trades are bounded by the owner's fixed floor, slippage
    // and size caps), and why no backup comes from this board's own past.
    let t = 1_793_960_000;
    let mut b = pump_board(t);
    let feed = hex::encode(real_curve().mint);
    post_curve(&mut b, t, 0);
    let honest = b.get_price_no_older_than(feed.clone(), 60).unwrap().price.0;
    post_curve(&mut b, t + 700, 184_404_837); // 0.18 SOL bought into the curve
    let pumped = b.get_price_no_older_than(feed.clone(), 60).unwrap().price.0;
    assert!(pumped > honest * 16 / 10, "the spot follows the curve: {honest} -> {pumped}");
    // An older read can't roll it back.
    let before = b.get_price_unsafe(feed.clone());
    post_curve(&mut b, t + 600, 0);
    assert_eq!(b.get_price_unsafe(feed), before);
}
