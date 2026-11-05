//! LP across a range of bins ("LP your bag"), Meteora-style, with a toggle:
//!   lock_in = false: classic two-way liquidity. Every swap that moves the price
//!                    through your bins pays you, in both directions, again and again.
//!   lock_in = true:  one-way take-profits. Liquidity that fills stays sold, so a
//!                    dump cannot trade your earnings back into the coin.
//! Tokens can only sit above the price (asks) and quote only below it (bids),
//! so a range that spans the price is split: tokens above, quote below.

use math::price::bins_for_multiple;
use math::wide::mul_div_down;
use math::{BPS, PRICE_SCALE};

use crate::{Book, Error, Side};

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

/// What an LP buy did: the positions it opened and the part it swapped.
#[derive(Debug, PartialEq, Eq)]
pub struct LpBuy {
    pub ids: Vec<u64>,
    /// Dust too small to place (see `lp_place`): handed back to the buyer.
    pub left_token: u128,
    pub left_quote: u128,
    pub swapped_in: u128, // quote spent buying coins (fee included)
    pub coins: u128,      // coins bought, now sitting above the price
    pub kept_quote: u128, // quote sitting below the price
}

impl Book {
    /// LP BUY: one atomic action that turns `quote` into two-way liquidity
    /// around the current price, from `down` (e.g. 7_000 = 0.7x) to `up`
    /// (e.g. 14_000 = 1.4x), over at most `n` bins. The part above the price
    /// is bought now (coins can only sit above it); the rest waits below as
    /// quote. Every bin gets about the same value, so the coin share is the
    /// share of the range that lies above the price.
    /// `min_coins`: slippage guard on the buy.
    pub fn lp_buy(
        &mut self, owner: &str, quote: u128, down: u32, up: u32, n: u32, min_coins: u128, now_ns: u64,
    ) -> Result<LpBuy, Error> {
        if quote == 0 {
            return Err(Error::ZeroAmount);
        }
        let (below, above) = self.range_bins(down, up, n)?;
        let buy = quote * above as u128 / (above + below) as u128;
        let r = self.swap(Side::Buy, buy, min_coins)?;
        let kept_quote = quote - r.used;
        let (ids, left_token, left_quote) = self.lp_place(owner, r.out, kept_quote, down, up, n, now_ns)?;
        Ok(LpBuy { ids, left_token, left_quote, swapped_in: r.used, coins: r.out, kept_quote })
    }

    /// Place `token` above and `quote` below the current price, inside
    /// [down, up] (multiples of the price, bps), over at most `n` bins, with no
    /// swap. Used by `lp_buy`, and to FOLLOW the price: when it leaves a range,
    /// the same assets are re-placed around the new price.
    /// A side worth under 0.1% of the total is dust: it is not placed but
    /// returned as (ids, unplaced token, unplaced quote) for the owner, so dust
    /// can never make a follow fail halfway.
    #[allow(clippy::too_many_arguments)]
    pub fn lp_place(
        &mut self, owner: &str, token: u128, quote: u128, down: u32, up: u32, n: u32, now_ns: u64,
    ) -> Result<(Vec<u64>, u128, u128), Error> {
        let (below, above) = self.range_bins(down, up, n)?;
        let a = self.active;
        let token_value = mul_div_down(token, self.price(a), PRICE_SCALE);
        let total = token_value + quote;
        let (mut token, mut quote, mut left_t, mut left_q) = (token, quote, 0, 0);
        if token_value * 1_000 < total {
            (left_t, token) = (token, 0);
        }
        if quote * 1_000 < total {
            (left_q, quote) = (quote, 0);
        }
        let n_above = match (token > 0, quote > 0) {
            (true, false) => n,
            (false, true) => 0,
            (false, false) => return Err(Error::ZeroAmount),
            (true, true) => ((n as i64 * above as i64 + (above + below) as i64 / 2) / (above + below) as i64).clamp(1, n as i64 - 1) as u32,
        };
        let mut ids = Vec::new();
        if token > 0 {
            ids.extend(self.lp_range(owner, a + 1, a + above, n_above, token, 0, false, 0, now_ns)?);
        }
        if quote > 0 {
            ids.extend(self.lp_range(owner, a - below, a - 1, n - n_above, 0, quote, false, 0, now_ns)?);
        }
        Ok((ids, left_t, left_q))
    }

    /// Bins below and above the price for a [down, up] range.
    fn range_bins(&self, down: u32, up: u32, n: u32) -> Result<(i32, i32), Error> {
        if !(1_000..BPS as u32).contains(&down) || up <= BPS as u32 || up > 100_000 || !(2..=MAX_RANGE_BINS).contains(&n) {
            return Err(Error::BadRange);
        }
        let step = self.cfg.step_bps;
        let below = bins_for_multiple(step, (BPS as u64 * BPS as u64 / down as u64) as u32).max(1);
        Ok((below, bins_for_multiple(step, up).max(1)))
    }
}

/// Split `total` into `parts` equal pieces; the last piece takes the remainder,
/// so nothing is lost to rounding.
fn share(total: u128, parts: usize, i: usize) -> u128 {
    let each = total / parts as u128;
    if i + 1 == parts { total - each * (parts as u128 - 1) } else { each }
}
