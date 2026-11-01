//! Swap fee: how much is charged, and who gets which slice.

use crate::wide::mul_div_up;
use crate::BPS;

/// Fee charged on `amount_in`, rounded UP (in favour of the pool).
pub fn fee_on(amount_in: u128, fee_bps: u16) -> u128 {
    mul_div_up(amount_in, fee_bps as u128, BPS)
}

/// How a fee is shared. Shares are in bps OF THE FEE (not of the trade).
/// LPs get whatever is left, so the three slices always add up exactly.
#[derive(Clone, Copy, Debug)]
pub struct FeeShares {
    pub stakers_bps: u16, // hold-to-earn vault
    pub buyback_bps: u16, // rule-based buyback + burn
}

#[derive(Debug, PartialEq, Eq)]
pub struct FeeSplit {
    pub lps: u128,     // whoever's liquidity was filled: treasury bins or ladder asks
    pub stakers: u128,
    pub buyback: u128,
}

/// Split a fee. Stakers and buyback round DOWN, LPs absorb the odd units.
pub fn split_fee(fee: u128, shares: FeeShares) -> FeeSplit {
    assert!(
        shares.stakers_bps as u128 + shares.buyback_bps as u128 <= BPS,
        "shares exceed 100%"
    );
    let stakers = fee * shares.stakers_bps as u128 / BPS;
    let buyback = fee * shares.buyback_bps as u128 / BPS;
    // Cannot underflow: stakers + buyback <= fee because shares <= 100%.
    let lps = fee - stakers - buyback;
    FeeSplit { lps, stakers, buyback }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHARES: FeeShares = FeeShares { stakers_bps: 4_000, buyback_bps: 1_000 };

    #[test]
    fn fee_rounds_up() {
        // 1_000_003 * 0.3% = 3000.009 -> 3001
        assert_eq!(fee_on(1_000_003, 30), 3001);
        assert_eq!(fee_on(1_000_000, 30), 3000); // exact, no rounding
        assert_eq!(fee_on(1, 30), 1); // any trade pays at least 1 unit
    }

    #[test]
    fn split_worked_example() {
        // 3001 -> stakers 40% = 1200.4 -> 1200, buyback 10% = 300.1 -> 300, LPs 1501
        let s = split_fee(3001, SHARES);
        assert_eq!(s, FeeSplit { lps: 1501, stakers: 1200, buyback: 300 });
    }

    #[test]
    fn split_is_always_exact() {
        // No unit is ever created or lost, for any fee size.
        for fee in [0u128, 1, 2, 7, 999, 3001, 123_456_789, u128::MAX / BPS] {
            let s = split_fee(fee, SHARES);
            assert_eq!(s.lps + s.stakers + s.buyback, fee);
        }
    }
}
