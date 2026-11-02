//! Take-profit ladder: part of a holder's tokens sit as one-way sell orders at
//! multiples of the current price. A rally sells them step by step, the holder
//! earns the maker fee slice, and a later dump cannot buy them back.

use math::price::bins_for_multiple;
use math::BPS;

use crate::{Book, Error, Side, SwapResult};

const DAY_NS: u64 = 86_400 * 1_000_000_000;

fn round_up_to_day(ns: u64) -> u64 {
    ns.div_ceil(DAY_NS) * DAY_NS
}

/// One rung: (price multiple in bps, share of tokens in bps). 15_000 = 1.5x.
pub type Rung = (u32, u16);

/// 50% kept as spot, 12.5% at each of 1.5x, 2x, 3x and 5x.
pub const DEFAULT_LADDER: [Rung; 4] = [(15_000, 1_250), (20_000, 1_250), (30_000, 1_250), (50_000, 1_250)];

/// What happens to a buy inside the launch window: same rungs, but locked.
pub const LAUNCH_LADDER: [Rung; 4] = DEFAULT_LADDER;

/// Result of a buy through the market.
#[derive(Debug)]
pub struct Buy {
    pub swap: SwapResult,
    pub kept: u128,        // tokens sent to the buyer's wallet
    pub ladder: Vec<u64>,  // take-profit positions created
    pub locked: bool,      // true if the launch-window rule applied
}

impl Book {
    /// Place `tokens` (already in the contract) on the ladder above the current price.
    /// Returns (position ids, tokens placed). Unplaced tokens stay with the caller.
    pub fn place_ladder(
        &mut self, owner: &str, tokens: u128, ladder: &[Rung], locked_until_ns: u64, now_ns: u64,
    ) -> Result<(Vec<u64>, u128), Error> {
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
            ids.push(self.place_take_profit(owner, bin, amount, locked_until_ns, now_ns)?);
            placed += amount;
        }
        Ok((ids, placed))
    }

    pub fn in_launch_window(&self, now_ns: u64) -> bool {
        now_ns < self.launched_ns.saturating_add(self.cfg.launch_window_ns)
    }

    /// The one way to buy. Outside the launch window the buyer's own `ladder`
    /// applies (pass &[] for plain spot). Inside it, the launch ladder is forced
    /// and locked for the creator-slice minimum, for EVERY buyer alike.
    pub fn buy(
        &mut self, owner: &str, quote_in: u128, min_out: u128, ladder: &[Rung], now_ns: u64,
    ) -> Result<Buy, Error> {
        let swap = self.swap(Side::Buy, quote_in, min_out)?;
        let locked = self.in_launch_window(now_ns);
        let (rungs, until) = if locked {
            // Rounded up to a whole day so one buyer's launch-day rungs share an
            // unlock time and merge (at most 24h longer than the minimum).
            (&LAUNCH_LADDER[..], round_up_to_day(now_ns + self.cfg.min_creator_lock_ns))
        } else {
            (ladder, 0)
        };
        let (ids, placed) = self.place_ladder(owner, swap.out, rungs, until, now_ns)?;
        Ok(Buy { kept: swap.out - placed, ladder: ids, locked, swap })
    }
}
