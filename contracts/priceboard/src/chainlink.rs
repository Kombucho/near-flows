//! Chainlink price feeds on EVM chains (AggregatorV3Interface, read through the
//! feed's proxy): what to call, how to read the answer, and the board's price
//! id for a feed. Pure functions; the contract (lib.rs) decides what to trust.
//!
//! latestRoundData() returns five 32-byte words:
//!   roundId (uint80), answer (int256), startedAt, updatedAt (unix s), answeredInRound.
//! decimals() returns one word: the answer is answer x 10^-decimals USD.

/// keccak256("latestRoundData()")[..4]
pub const LATEST_ROUND_DATA: [u8; 4] = [0xfe, 0xaf, 0x96, 0x8c];
/// keccak256("decimals()")[..4]
pub const DECIMALS: [u8; 4] = [0x31, 0x3c, 0xe5, 0x67];

#[derive(Debug, Clone, PartialEq)]
pub struct Round {
    pub round_id: u128,
    pub answer: i64,
    pub updated_at: u64,
}

/// An unsigned word that must fit in `bits`; None if anything above is set.
fn word(w: &[u8], bits: u32) -> Option<u128> {
    let skip = 32 - (bits as usize).div_ceil(8);
    if w.len() != 32 || w[..skip].iter().any(|b| *b != 0) {
        return None;
    }
    let v = w[skip..].iter().fold(0u128, |a, b| (a << 8) | *b as u128);
    (bits >= 128 || v >> bits == 0).then_some(v)
}

/// The latest round. Refuses a non-positive answer (int256's sign bit set, or
/// zero), one too big for the board's i64, and anything that isn't exactly five words.
pub fn decode_round(b: &[u8]) -> Result<Round, &'static str> {
    if b.len() != 5 * 32 {
        return Err("latestRoundData: not 5 words");
    }
    let at = |i: usize| &b[i * 32..(i + 1) * 32];
    let round_id = word(at(0), 80).ok_or("latestRoundData: bad round id")?;
    // A negative int256 has its top bit set: word() refuses it like any oversized value.
    let answer = word(at(1), 63).ok_or("answer is negative or too large")? as i64;
    if answer == 0 {
        return Err("answer is zero");
    }
    let updated_at = word(at(3), 64).ok_or("latestRoundData: bad updatedAt")? as u64;
    Ok(Round { round_id, answer, updated_at })
}

/// decimals(): at most 18 (every Chainlink USD feed uses 8 or 18).
pub fn decode_decimals(b: &[u8]) -> Result<u8, &'static str> {
    match word(b, 8) {
        Some(d) if d <= 18 => Ok(d as u8),
        _ => Err("decimals: not a uint8 <= 18"),
    }
}

/// The board's price id for a feed: Wormhole chain id (2 bytes, big-endian),
/// 10 zero bytes, the feed's proxy address (20 bytes). Anyone can read off the
/// id which chain and which contract a price comes from.
pub fn price_id(chain: u16, address: &[u8; 20]) -> [u8; 32] {
    let mut id = [0u8; 32];
    id[..2].copy_from_slice(&chain.to_be_bytes());
    id[12..].copy_from_slice(address);
    id
}
