//! Real vectors where they exist (tests-data/, all public mainnet data read
//! 2026-11-06): two query responses the mainnet guardians signed (set 4), the
//! VAAs in which set 4 approved 5, 5 approved 6 and 6 approved 7, set 7 as
//! Ethereum's core contract reports it today, and a live pump.fun curve account.
//! A signed sol_pda response needs a Wormhole Query API key (none here), so that
//! one is built from the real curve bytes and signed by test guardian keys.

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
