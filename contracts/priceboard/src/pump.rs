//! pump.fun bonding curves: the account at PDA ["bonding-curve", mint] of the
//! pump program holds the curve's virtual reserves; the spot price is
//! virtual quote / virtual token (a constant-product curve). Layout from
//! @pump-fun/pump-sdk 4.3.1 (every older layout is a prefix of the 166-byte one):
//!   0  discriminator sha256("account:BondingCurve")[..8]
//!   8  virtual_token_reserves u64 LE    16 virtual_quote_reserves u64 LE
//!  24  real_token_reserves             32 real_quote_reserves
//!  40  token_total_supply              48 complete (bool)
//!  49  creator [32]                    81 is_mayhem_mode   82 is_cashback_coin
//!  83  quote_mint [32] (all zero = SOL; absent on curves older than the field)

use math::wide::U256;

/// 6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P
pub const PROGRAM: [u8; 32] = hex32("0156e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b0");
pub const DISCRIMINATOR: [u8; 8] = [23, 183, 248, 55, 96, 216, 172, 96];
pub const SEED: &[u8] = b"bonding-curve";
/// Every pump.fun coin has 6 decimals.
pub const COIN_DECIMALS: u32 = 6;
/// Prices the board writes: whole-coin USD price x 1e12 (memecoins trade far below a cent).
pub const EXPO: i32 = -12;

#[derive(Debug, Clone, PartialEq)]
pub struct Curve {
    pub virtual_token: u64,
    pub virtual_quote: u64,
    pub complete: bool,
    pub quote_mint: [u8; 32],
}

pub fn decode(data: &[u8]) -> Result<Curve, &'static str> {
    if data.len() < 49 || data[..8] != DISCRIMINATOR {
        return Err("not a pump.fun bonding curve");
    }
    let u = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
    let quote_mint = if data.len() >= 115 { data[83..115].try_into().unwrap() } else { [0; 32] };
    Ok(Curve { virtual_token: u(8), virtual_quote: u(16), complete: data[48] != 0, quote_mint })
}

/// USD per whole coin x 1e12, from the curve and the quote token's USD price
/// (Pyth: `q_price` x 10^`q_expo`, `q_decimals` = the quote token's decimals):
///   (vq / 10^qd) / (vt / 10^6) x q_price x 10^q_expo, times 10^12.
/// One division at the end (in 256 bits), so nothing is lost on tiny prices.
pub fn usd_e12(c: &Curve, q_price: i64, q_expo: i32, q_decimals: u32) -> Option<i64> {
    if c.virtual_token == 0 || c.virtual_quote == 0 || q_price <= 0 {
        return None;
    }
    let mut num = U256::from(c.virtual_quote) * U256::from(q_price as u64) * U256::from(10u64).pow(U256::from(COIN_DECIMALS));
    let mut den = U256::from(c.virtual_token) * U256::from(10u64).pow(U256::from(q_decimals));
    // x 10^(12 + q_expo): multiply or divide.
    let e = -EXPO + q_expo;
    if e >= 0 {
        num *= U256::from(10u64).checked_pow(U256::from(e as u32))?;
    } else {
        den *= U256::from(10u64).checked_pow(U256::from((-e) as u32))?;
    }
    let r = num / den;
    (r <= U256::from(i64::MAX as u64)).then(|| r.as_u64() as i64)
}

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        let c = b[i];
        i += 1;
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("hex"),
        };
        out[n / 2] |= if n % 2 == 0 { v << 4 } else { v };
        n += 1;
    }
    assert!(n == 64, "32 bytes of hex");
    out
}
