//! The order book of bins and the liquidity positions inside them.

use std::collections::BTreeMap;

use math::fees::FeeShares;
use math::price::bin_price;
use math::wide::mul_div_down;
use math::PRICE_SCALE;

use crate::Error;

/// One price level. Holds tokens (for buyers) and quote (for sellers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bin {
    pub token: u128,
    pub quote: u128,
    /// Total LP shares in this bin. Shares = claim on the bin's reserves.
    pub shares: u128,
    /// Fee-per-share counters (see math::acc), one per currency.
    pub acc_quote: u128,
    pub acc_token: u128,
}

/// Fixed at creation. None of this can change afterwards.
#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub base_price: u128, // price of bin 0, quote per token, fixed-point
    pub step_bps: u16,    // price gap between bins (100 = 1%)
    pub fee_bps: u16,     // swap fee (100 = 1%)
    pub shares: FeeShares,
}

/// One LP's liquidity in one bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub owner: String,
    pub bin: i32,
    pub shares: u128,
    pub snap_quote: u128, // fee counters when this position last settled
    pub snap_token: u128,
    pub locked_until_ns: u64, // 0 = not locked
}

/// Fee slices that do not go to LPs, waiting to be distributed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pots {
    pub pot_quote: u128,
    pub pot_token: u128,
    pub creator_quote: u128,
    pub creator_token: u128,
}

#[derive(Clone, Debug)]
pub struct Book {
    pub cfg: Config,
    pub active: i32, // the bin where the current price sits
    // BTreeMap keeps bins sorted by id, so "next bin with liquidity above/below"
    // is a cheap ordered walk that skips empty price levels entirely.
    pub bins: BTreeMap<i32, Bin>,
    pub positions: BTreeMap<u64, Position>,
    pub next_id: u64,
    pub pots: Pots,
}

impl Book {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.fee_bps < 10_000, "fee must be below 100%");
        Book { cfg, active: 0, bins: BTreeMap::new(), positions: BTreeMap::new(), next_id: 0, pots: Pots::default() }
    }

    pub fn price(&self, bin: i32) -> u128 {
        bin_price(self.cfg.base_price, self.cfg.step_bps, bin)
    }

    /// Value of (token, quote) at bin `bin`'s price, in quote units.
    fn value(&self, bin: i32, token: u128, quote: u128) -> u128 {
        mul_div_down(token, self.price(bin), PRICE_SCALE) + quote
    }

    /// Put liquidity into one bin. Returns the new position id.
    pub fn add_liquidity(
        &mut self, owner: &str, bin: i32, token: u128, quote: u128, locked_until_ns: u64,
    ) -> Result<u64, Error> {
        if token == 0 && quote == 0 {
            return Err(Error::ZeroAmount);
        }
        if (bin > self.active && quote > 0) || (bin < self.active && token > 0) {
            return Err(Error::WrongSide);
        }
        let value = self.value(bin, token, quote);
        let existing = self.bins.get(&bin).cloned().unwrap_or_default();
        let bin_value = self.value(bin, existing.token, existing.quote);
        // First LP in an (empty) bin: 1 share per unit of value. Later LPs: pro-rata.
        let minted = if existing.shares == 0 || bin_value == 0 {
            value
        } else {
            mul_div_down(value, existing.shares, bin_value)
        };
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        let b = self.bins.entry(bin).or_default();
        b.token += token;
        b.quote += quote;
        b.shares += minted;
        let pos = Position {
            owner: owner.to_string(), bin, shares: minted,
            snap_quote: b.acc_quote, snap_token: b.acc_token, locked_until_ns,
        };
        let id = self.next_id;
        self.positions.insert(id, pos);
        self.next_id += 1;
        Ok(id)
    }

    /// Spread liquidity evenly over `n` bins on each side of the active bin:
    /// tokens in active+1..=active+n (asks), quote in active-n..=active-1 (bids).
    /// This is what `seed()` and `graduate()` will use. Returns the position ids.
    pub fn add_uniform(
        &mut self, owner: &str, token: u128, quote: u128, n: i32, locked_until_ns: u64,
    ) -> Result<Vec<u64>, Error> {
        assert!(n > 0, "need at least one bin per side");
        let (t_each, q_each) = (token / n as u128, quote / n as u128);
        let mut ids = Vec::new();
        for i in 1..=n {
            if t_each > 0 {
                ids.push(self.add_liquidity(owner, self.active + i, t_each, 0, locked_until_ns)?);
            }
            if q_each > 0 {
                ids.push(self.add_liquidity(owner, self.active - i, 0, q_each, locked_until_ns)?);
            }
        }
        Ok(ids)
    }
}
