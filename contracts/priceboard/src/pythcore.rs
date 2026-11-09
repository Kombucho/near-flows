//! Pyth Core price updates, checked here instead of by Pyth's NEAR receiver
//! (which refuses every update since Pyth's Aug 2026 Core upgrade).
//!
//! What Hermes serves (`/v2/updates/price/...`, an "accumulator update"):
//!   "PNAU", major 1, minor, trailing header (skipped), update type 0 (Wormhole Merkle),
//!   u16 length + a VAA whose payload is a Merkle root:
//!     "AUWV", type 0, slot u64, ring size u32, root (20 bytes)
//!   u8 count, then per price: u16 length + message, u8 proof length + 20-byte nodes.
//! A price message: type 0, feed id (32), price i64, conf u64, expo i32,
//! publish_time i64, prev_publish_time i64, EMA price i64, EMA conf u64.
//!
//! The Merkle tree is Pyth's keccak160 one (pythnet_sdk accumulators/merkle.rs):
//! leaf = keccak256(0x00 || message)[..20], node = keccak256(0x01 || lower || higher)[..20],
//! the two children ordered as byte strings, so a proof is just the siblings.
//!
//! Who signs: since the upgrade, not Wormhole's 19 guardians but Pyth's own 5
//! routers, as guardian set 1 of Pyth's wormhole receivers, 3 of 5 needed (half
//! plus one). The VAA's emitter is chain 26 (Pythnet), address "PythnetPythnet...".
//! All checked against real updates in the tests (tests-data/pyth_core.json).

use crate::wormhole::{parse_vaa, verify_sigs, Reader};
use near_sdk::env;

pub const MAGIC: &[u8] = b"PNAU";
pub const ROOT_MAGIC: &[u8] = b"AUWV";
/// Pythnet's Wormhole chain id: the emitter chain of every Pyth Core root.
pub const PYTHNET: u16 = 26;

/// Pyth's quorum for its own signer set: more than half (3 of 5). Pyth's
/// receivers on EVM chains take 3 of today's 5 and answer "no quorum" to 2.
pub fn quorum(n: usize) -> usize {
    n / 2 + 1
}

/// An accumulator update, split (nothing checked yet).
pub struct Update<'a> {
    pub vaa: &'a [u8],
    /// (message, Merkle proof) per price.
    pub messages: Vec<(&'a [u8], Vec<[u8; 20]>)>,
}

pub fn parse_update(b: &[u8]) -> Result<Update<'_>, &'static str> {
    let mut r = Reader::new(b);
    if r.bytes(4)? != MAGIC {
        return Err("not a Pyth accumulator update (PNAU)");
    }
    if r.u8()? != 1 {
        return Err("accumulator update: major version 1 only");
    }
    r.u8()?; // minor: newer minors only add header bytes, skipped next
    let trailing = r.u8()? as usize;
    r.bytes(trailing)?;
    if r.u8()? != 0 {
        return Err("accumulator update: Wormhole Merkle updates only");
    }
    let n = r.u16()? as usize;
    let vaa = r.bytes(n)?;
    let count = r.u8()?;
    if count == 0 {
        return Err("no prices in the update");
    }
    let mut messages = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let len = r.u16()? as usize;
        let m = r.bytes(len)?;
        let depth = r.u8()?;
        let mut proof = Vec::with_capacity(depth as usize);
        for _ in 0..depth {
            proof.push(r.bytes(20)?.try_into().unwrap());
        }
        messages.push((m, proof));
    }
    r.done()?;
    Ok(Update { vaa, messages })
}

/// The Merkle root a VAA's payload carries, and the Pythnet slot it's for.
pub fn merkle_root(payload: &[u8]) -> Result<([u8; 20], u64), &'static str> {
    let mut r = Reader::new(payload);
    if r.bytes(4)? != ROOT_MAGIC || r.u8()? != 0 {
        return Err("not a Pyth Merkle root (AUWV)");
    }
    let slot = r.u64()?;
    r.u32()?; // ring size
    let root = r.bytes(20)?.try_into().unwrap();
    r.done()?;
    Ok((root, slot))
}

fn keccak160(parts: &[&[u8]]) -> [u8; 20] {
    env::keccak256_array(&parts.concat())[..20].try_into().unwrap()
}

/// Does `message` sit under `root`, by `proof`?
pub fn proven(message: &[u8], proof: &[[u8; 20]], root: &[u8; 20]) -> bool {
    let mut h = keccak160(&[&[0], message]);
    for s in proof {
        h = if h <= *s { keccak160(&[&[1], &h, s]) } else { keccak160(&[&[1], s, &h]) };
    }
    h == *root
}

#[derive(Debug, Clone, PartialEq)]
pub struct PriceMsg {
    pub id: [u8; 32],
    pub price: i64,
    pub conf: u64,
    pub expo: i32,
    pub publish_time: i64,
}

/// A price feed message (type 0). Pyth's receivers read the fields they know
/// and leave any bytes after them for later versions; so does this.
pub fn parse_price(m: &[u8]) -> Result<PriceMsg, &'static str> {
    let mut r = Reader::new(m);
    if r.u8()? != 0 {
        return Err("not a price message");
    }
    let id = r.b32()?;
    let price = r.u64()? as i64;
    let conf = r.u64()?;
    let expo = r.u32()? as i32;
    let publish_time = r.u64()? as i64;
    r.bytes(8 + 8 + 8)?; // prev publish time, EMA price, EMA conf: unused, but must be there
    Ok(PriceMsg { id, price, conf, expo, publish_time })
}

/// What a verified update says: the root's slot and VAA sequence, and its prices.
#[derive(Debug, Clone, PartialEq)]
pub struct Verified {
    pub slot: u64,
    pub sequence: u64,
    pub prices: Vec<PriceMsg>,
}

/// Check an update end to end: the VAA signed by a quorum (more than half) of
/// guardian set `set_index`, from `emitter_chain`/`emitter`, its payload a
/// Merkle root, and every price message proven under that root.
pub fn verify(update: &[u8], set_index: u32, guardians: &[[u8; 20]], emitter_chain: u16, emitter: &[u8; 32]) -> Result<Verified, &'static str> {
    let u = parse_update(update)?;
    let v = parse_vaa(u.vaa)?;
    if v.guardian_set != set_index {
        return Err("VAA not signed by the board's guardian set");
    }
    verify_sigs(&v.digest, &v.sigs, guardians, quorum)?;
    if v.emitter_chain != emitter_chain || v.emitter != *emitter {
        return Err("not from Pyth's emitter");
    }
    let (root, slot) = merkle_root(v.payload)?;
    let mut prices = Vec::with_capacity(u.messages.len());
    for (m, proof) in &u.messages {
        if !proven(m, proof, &root) {
            return Err("bad Merkle proof");
        }
        prices.push(parse_price(m)?);
    }
    Ok(Verified { slot, sequence: v.sequence, prices })
}
