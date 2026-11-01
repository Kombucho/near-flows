//! Swap fee: how much is charged, and who gets which slice.
//!
//! Every fee splits three ways:
//!   - LPs:     whoever's liquidity was filled (treasury, locked LPs, ladder asks)
//!   - creator: shared by LOCKED liquidity, pro-rata (falls back to the pot if none)
//!   - pot:     the community pot; stakers steer it between dividend / burn / deepen

use crate::wide::mul_div_up;
use crate::BPS;

/// Fee charged on `amount_in`, rounded UP (in favour of the pool).
pub fn fee_on(amount_in: u128, fee_bps: u16) -> u128 {
    mul_div_up(amount_in, fee_bps as u128, BPS)
}

/// Fee to add on top of a NET amount so that fee / (net + fee) == fee_bps.
/// Used when a bin is emptied exactly: we know the net cost and need the gross.
pub fn fee_on_net(net: u128, fee_bps: u16) -> u128 {
    mul_div_up(net, fee_bps as u128, BPS - fee_bps as u128)
}

/// Shares in bps OF THE FEE (not of the trade). LPs get the remainder.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct FeeShares {
    pub pot_bps: u16,
    pub creator_bps: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FeeSplit {
    pub lps: u128,
    pub pot: u128,
    pub creator: u128,
}

/// Split a fee. Named slices round DOWN, LPs absorb the odd units.
pub fn split_fee(fee: u128, shares: FeeShares) -> FeeSplit {
    let named = shares.pot_bps as u128 + shares.creator_bps as u128;
    assert!(named <= BPS, "shares exceed 100%");
    let pot = fee * shares.pot_bps as u128 / BPS;
    let creator = fee * shares.creator_bps as u128 / BPS;
    // Cannot underflow: named slices sum to at most `fee` because shares <= 100%.
    let lps = fee - pot - creator;
    FeeSplit { lps, pot, creator }
}

/// Stake-weighted vote totals for the three fixed pot options.
/// Each total = sum over stakers of (stake * that staker's bps for the option).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct PotVotes {
    pub dividend: u128,
    pub burn: u128,
    pub deepen: u128,
}

/// Mix used before anyone votes: the old 35% dividend / 10% burn ratio (7778 / 2222).
pub const DEFAULT_MIX: PotVotes = PotVotes { dividend: 7_778, burn: 2_222, deepen: 0 };

#[derive(Debug, PartialEq, Eq)]
pub struct PotSplit {
    pub dividend: u128,
    pub burn: u128,
    pub deepen: u128,
}

/// Split the pot by the current votes. Burn and deepen round DOWN,
/// dividend absorbs the odd units (it goes back to all stakers anyway).
pub fn split_pot(pot: u128, votes: PotVotes) -> PotSplit {
    let v = if votes == PotVotes::default() { DEFAULT_MIX } else { votes };
    let total = v.dividend + v.burn + v.deepen;
    let burn = crate::wide::mul_div_down(pot, v.burn, total);
    let deepen = crate::wide::mul_div_down(pot, v.deepen, total);
    PotSplit { dividend: pot - burn - deepen, burn, deepen }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Proposal: pot 45%, creator 10%, LPs the remaining 45%.
    const SHARES: FeeShares = FeeShares { pot_bps: 4_500, creator_bps: 1_000 };

    #[test]
    fn fee_rounds_up() {
        // 1_000_003 * 0.3% = 3000.009 -> 3001
        assert_eq!(fee_on(1_000_003, 30), 3001);
        assert_eq!(fee_on(1_000_000, 30), 3000); // exact, no rounding
        assert_eq!(fee_on(1, 30), 1); // any trade pays at least 1 unit
    }

    #[test]
    fn fee_on_net_matches_fee_on_gross() {
        // 1% fee: net 990 needs gross 1000, i.e. fee 10.
        assert_eq!(fee_on_net(990, 100), 10);
        assert_eq!(fee_on(1_000, 100), 10);
    }

    #[test]
    fn split_worked_example() {
        // 3001 -> pot 45% = 1350.45 -> 1350, creator 10% = 300.1 -> 300, LPs 1351
        let s = split_fee(3001, SHARES);
        assert_eq!(s, FeeSplit { lps: 1351, pot: 1350, creator: 300 });
    }

    #[test]
    fn split_is_always_exact() {
        for fee in [0u128, 1, 2, 7, 999, 3001, 123_456_789, u128::MAX / BPS] {
            let s = split_fee(fee, SHARES);
            assert_eq!(s.lps + s.pot + s.creator, fee);
        }
    }

    #[test]
    #[should_panic(expected = "shares exceed 100%")]
    fn rejects_shares_over_100_percent() {
        split_fee(1_000, FeeShares { pot_bps: 9_500, creator_bps: 1_000 });
    }

    #[test]
    fn pot_alice_and_bob() {
        // Alice: 300 staked, 100% burn. Bob: 100 staked, 100% dividend.
        let votes = PotVotes { dividend: 100 * 10_000, burn: 300 * 10_000, deepen: 0 };
        assert_eq!(split_pot(1_000, votes), PotSplit { dividend: 250, burn: 750, deepen: 0 });
    }

    #[test]
    fn pot_defaults_before_any_vote() {
        // 1000 * 2222/10000 = 222.2 -> 222 burn, dividend takes the rest.
        let s = split_pot(1_000, PotVotes::default());
        assert_eq!(s, PotSplit { dividend: 778, burn: 222, deepen: 0 });
    }

    #[test]
    fn pot_split_is_always_exact() {
        let votes = PotVotes { dividend: 7, burn: 11, deepen: 13 };
        for pot in [0u128, 1, 30, 999, 1_000_003] {
            let s = split_pot(pot, votes);
            assert_eq!(s.dividend + s.burn + s.deepen, pot);
        }
    }
}
