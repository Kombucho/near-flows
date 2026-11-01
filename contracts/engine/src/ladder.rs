//! Take-profit ladder: part of a holder's tokens sit as asks at multiples of
//! the current price, so a rally sells them step by step (and earns LP fees).

use math::price::bins_for_multiple;
use math::BPS;

use crate::{Book, Error, Side, SwapResult};

/// One rung: (price multiple in bps, share of tokens in bps). 15_000 = 1.5x.
pub type Rung = (u32, u16);

/// 50% kept as spot, 12.5% at each of 1.5x, 2x, 3x and 5x.
pub const DEFAULT_LADDER: [Rung; 4] = [(15_000, 1_250), (20_000, 1_250), (30_000, 1_250), (50_000, 1_250)];

impl Book {
    /// Place `tokens` (already in the contract) on the ladder above the current price.
    /// Returns (position ids, tokens placed). Unplaced tokens stay with the caller.
    pub fn place_ladder(&mut self, owner: &str, tokens: u128, ladder: &[Rung], now_ns: u64) -> Result<(Vec<u64>, u128), Error> {
        let share_sum: u128 = ladder.iter().map(|r| r.1 as u128).sum();
        assert!(share_sum <= BPS, "ladder shares exceed 100%");
        let (mut ids, mut placed) = (Vec::new(), 0);
        for &(multiple_bps, share_bps) in ladder {
            assert!(multiple_bps > BPS as u32, "a take-profit must be above the price");
            let amount = tokens * share_bps as u128 / BPS;
            if amount == 0 {
                continue;
            }
            let bin = self.active + bins_for_multiple(self.cfg.step_bps, multiple_bps);
            ids.push(self.add_liquidity(owner, bin, amount, 0, 0, now_ns)?);
            placed += amount;
        }
        Ok((ids, placed))
    }

    /// Buy, then put part of what you bought on the ladder.
    /// Returns (swap result, tokens kept by the buyer, ladder position ids).
    pub fn buy_with_ladder(
        &mut self, owner: &str, quote_in: u128, min_out: u128, ladder: &[Rung], now_ns: u64,
    ) -> Result<(SwapResult, u128, Vec<u64>), Error> {
        let r = self.swap(Side::Buy, quote_in, min_out)?;
        let (ids, placed) = self.place_ladder(owner, r.out, ladder, now_ns)?;
        let kept = r.out - placed;
        Ok((r, kept, ids))
    }
}
