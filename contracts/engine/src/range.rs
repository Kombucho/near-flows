//! LP across a range of bins ("LP your bag"), Meteora-style, with a toggle:
//!   lock_in = false: classic two-way liquidity. Every swap that moves the price
//!                    through your bins pays you, in both directions, again and again.
//!   lock_in = true:  one-way take-profits. Liquidity that fills stays sold, so a
//!                    dump cannot trade your earnings back into the coin.
//! Tokens can only sit above the price (asks) and quote only below it (bids),
//! so a range that spans the price is split: tokens above, quote below.

use crate::{Book, Error};

/// Most bins one range may use (bounds storage and gas per call).
pub const MAX_RANGE_BINS: u32 = 20;

/// `n` bins evenly spread over [from, to], both ends included, no duplicates.
pub fn spread(from: i32, to: i32, n: u32) -> Vec<i32> {
    if from > to || n == 0 {
        return vec![];
    }
    let width = (to - from) as i64;
    let n = n.min(width as u32 + 1) as i64;
    let mut bins: Vec<i32> = (0..n).map(|i| from + if n == 1 { 0 } else { (width * i / (n - 1)) as i32 }).collect();
    bins.dedup();
    bins
}

impl Book {
    /// Put `token` and/or `quote` into the range [from, to] over at most `n` bins.
    /// Returns the position ids created (or topped up, for lock-in rungs).
    #[allow(clippy::too_many_arguments)]
    pub fn lp_range(
        &mut self, owner: &str, from: i32, to: i32, n: u32, token: u128, quote: u128,
        lock_in: bool, locked_until_ns: u64, now_ns: u64,
    ) -> Result<Vec<u64>, Error> {
        if n == 0 || n > MAX_RANGE_BINS || from > to {
            return Err(Error::BadRange);
        }
        if lock_in && quote > 0 {
            return Err(Error::WrongSide); // lock-in rungs are sell orders: coins only
        }
        let asks = spread(from.max(self.active + 1), to, n);
        let bids = spread(from, to.min(self.active - 1), n);
        if (token > 0 && asks.is_empty()) || (quote > 0 && bids.is_empty()) {
            return Err(Error::WrongSide);
        }
        let mut ids = Vec::new();
        for (i, &bin) in asks.iter().enumerate() {
            let part = share(token, asks.len(), i);
            if part == 0 { continue; }
            ids.push(if lock_in {
                self.place_take_profit(owner, bin, part, locked_until_ns, now_ns)?
            } else {
                self.add_liquidity(owner, bin, part, 0, locked_until_ns, now_ns)?
            });
        }
        for (i, &bin) in bids.iter().enumerate() {
            let part = share(quote, bids.len(), i);
            if part == 0 { continue; }
            ids.push(self.add_liquidity(owner, bin, 0, part, locked_until_ns, now_ns)?);
        }
        Ok(ids)
    }
}

/// Split `total` into `parts` equal pieces; the last piece takes the remainder,
/// so nothing is lost to rounding.
fn share(total: u128, parts: usize, i: usize) -> u128 {
    let each = total / parts as u128;
    if i + 1 == parts { total - each * (parts as u128 - 1) } else { each }
}
