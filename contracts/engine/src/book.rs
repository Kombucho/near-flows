//! The order book of bins and the liquidity positions inside them.

use std::collections::{BTreeMap, BTreeSet};

use math::fees::FeeShares;
use math::price::bin_price;
use math::wide::mul_div_down;
use math::PRICE_SCALE;

use crate::vault::Vault;
use crate::Error;

/// Owner name of liquidity the protocol itself holds. It is locked forever
/// and never earns the creator slice.
pub const PROTOCOL: &str = "@protocol";

const HOUR_NS: u64 = 3_600 * 1_000_000_000;
const DAY_NS: u64 = 24 * HOUR_NS;

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
    pub crank_interval_ns: u64,   // how often burn/deepen/dividends may run
    pub unstake_delay_ns: u64,    // wait between "unstake" and getting tokens back
    pub min_creator_lock_ns: u64, // lock needed to earn the creator slice
    pub deepen_bins: i32,         // bins per side that "deepen" spreads over
    /// Launch window after graduation. Inside it, part of every buy becomes a
    /// LOCKED take-profit ladder, so snipers and bundlers cannot dump on day one:
    /// their bag turns into depth that earns the creator slice instead. 0 = off.
    pub launch_window_ns: u64,
}

impl Config {
    /// The defaults we agreed: 1% bins, 45/10/45 split, hourly crank,
    /// 7-day unstake delay, 30-day minimum lock for the creator slice.
    pub fn standard(base_price: u128, fee_bps: u16) -> Self {
        Config {
            base_price,
            step_bps: 100,
            fee_bps,
            shares: FeeShares { pot_bps: 4_500, creator_bps: 1_000 },
            crank_interval_ns: HOUR_NS,
            unstake_delay_ns: 7 * DAY_NS,
            min_creator_lock_ns: 30 * DAY_NS,
            deepen_bins: 10,
            launch_window_ns: DAY_NS,
        }
    }
}

/// One LP's liquidity in one bin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    pub owner: String,
    pub bin: i32,
    pub shares: u128,
    pub snap_quote: u128, // LP fee counters when this position last settled
    pub snap_token: u128,
    pub locked_until_ns: u64, // 0 = not locked
    /// Weight in the creator slice (its value at deposit), 0 if not eligible.
    pub creator_weight: u128,
    pub snap_cq: u128, // creator counters when last settled
    pub snap_ct: u128,
    pub creator_owed_q: u128, // creator slice settled but not yet claimed
    pub creator_owed_t: u128,
}

/// Fee slices that do not go to LPs, waiting for the next crank.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pots {
    pub pot_quote: u128,
    pub pot_token: u128,
    pub creator_quote: u128,
    pub creator_token: u128,
}

/// The creator slice is shared by locked liquidity, pro-rata to weight.
#[derive(Clone, Debug, Default)]
pub struct CreatorPool {
    pub total_weight: u128,
    pub acc_q: u128,
    pub acc_t: u128,
    pub locked: BTreeSet<u64>, // position ids that currently carry weight
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
    pub creator: CreatorPool,
    pub vault: Vault,
    pub protocol_pos: BTreeMap<i32, u64>, // bin -> the protocol's position there
    pub burn_quote: u128,                 // quote waiting to buy back + burn
    pub burned: u128,                     // tokens burned so far
    pub last_crank_ns: u64,
    pub launched_ns: u64, // when the market opened (graduation or seed)
    /// Slow reference price (as a bin id): each crank it moves 1/4 of the way
    /// toward the active bin. Buyback and deepen only act near it, so a price
    /// pushed for one hour cannot steer the protocol's money.
    pub ref_bin: i32,
}

impl Book {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.fee_bps < 10_000, "fee must be below 100%");
        assert!(cfg.base_price > 0, "price must be positive");
        Book {
            cfg, active: 0, bins: BTreeMap::new(), positions: BTreeMap::new(), next_id: 0,
            pots: Pots::default(), creator: CreatorPool::default(), vault: Vault::default(),
            protocol_pos: BTreeMap::new(), burn_quote: 0, burned: 0, last_crank_ns: 0, launched_ns: 0,
            ref_bin: 0,
        }
    }

    pub fn price(&self, bin: i32) -> u128 {
        bin_price(self.cfg.base_price, self.cfg.step_bps, bin)
    }

    /// Value of (token, quote) at bin `bin`'s price, in quote units.
    pub fn value(&self, bin: i32, token: u128, quote: u128) -> u128 {
        mul_div_down(token, self.price(bin), PRICE_SCALE) + quote
    }

    /// Shares to mint for depositing `value` into `bin`.
    fn shares_for(&self, bin: i32, value: u128) -> u128 {
        let b = self.bins.get(&bin).cloned().unwrap_or_default();
        let bin_value = self.value(bin, b.token, b.quote);
        // First LP in an (empty) bin: 1 share per unit of value. Later LPs: pro-rata.
        if b.shares == 0 || bin_value == 0 { value } else { mul_div_down(value, b.shares, bin_value) }
    }

    fn check_side(&self, bin: i32, token: u128, quote: u128) -> Result<(), Error> {
        if token == 0 && quote == 0 {
            return Err(Error::ZeroAmount);
        }
        if (bin > self.active && quote > 0) || (bin < self.active && token > 0) {
            return Err(Error::WrongSide);
        }
        Ok(())
    }

    /// Put liquidity into one bin. Returns the new position id.
    pub fn add_liquidity(
        &mut self, owner: &str, bin: i32, token: u128, quote: u128, locked_until_ns: u64, now_ns: u64,
    ) -> Result<u64, Error> {
        self.check_side(bin, token, quote)?;
        let value = self.value(bin, token, quote);
        let minted = self.shares_for(bin, value);
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        let eligible = owner != PROTOCOL
            && locked_until_ns >= now_ns.saturating_add(self.cfg.min_creator_lock_ns);
        let weight = if eligible { value } else { 0 };
        let b = self.bins.entry(bin).or_default();
        b.token += token;
        b.quote += quote;
        b.shares += minted;
        let pos = Position {
            owner: owner.to_string(), bin, shares: minted,
            snap_quote: b.acc_quote, snap_token: b.acc_token, locked_until_ns,
            creator_weight: weight, snap_cq: self.creator.acc_q, snap_ct: self.creator.acc_t,
            creator_owed_q: 0, creator_owed_t: 0,
        };
        let id = self.next_id;
        self.next_id += 1;
        self.positions.insert(id, pos);
        if weight > 0 {
            self.creator.total_weight += weight;
            self.creator.locked.insert(id);
        }
        Ok(id)
    }

    /// Add protocol liquidity to a bin, topping up its existing position there.
    pub(crate) fn add_protocol(&mut self, bin: i32, token: u128, quote: u128) -> Result<(), Error> {
        let Some(&id) = self.protocol_pos.get(&bin) else {
            let id = self.add_liquidity(PROTOCOL, bin, token, quote, u64::MAX, 0)?;
            self.protocol_pos.insert(bin, id);
            return Ok(());
        };
        self.check_side(bin, token, quote)?;
        // Settle fees BEFORE adding shares: otherwise the new shares would carry an
        // old snapshot and could claim fees earned before they existed.
        let (fq, ft) = self.claim(id, PROTOCOL)?;
        self.pots.pot_quote += fq;
        self.pots.pot_token += ft;
        let minted = self.shares_for(bin, self.value(bin, token, quote));
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        let b = self.bins.get_mut(&bin).expect("protocol bin exists");
        b.token += token;
        b.quote += quote;
        b.shares += minted;
        self.positions.get_mut(&id).expect("protocol position exists").shares += minted;
        Ok(())
    }

    /// Spread protocol liquidity over `n` bins per side of the active bin:
    /// tokens in active+1..=active+n (asks), quote in active-n..=active-1 (bids).
    pub(crate) fn add_protocol_uniform(&mut self, token: u128, quote: u128, n: i32) {
        assert!(n > 0, "need at least one bin per side");
        let (t_each, q_each) = (token / n as u128, quote / n as u128);
        let center = self.active;
        for i in 1..=n {
            if t_each > 0 {
                self.add_protocol(center + i, t_each, 0).expect("asks above price");
            }
            if q_each > 0 {
                self.add_protocol(center - i, 0, q_each).expect("bids below price");
            }
        }
    }

    /// Spread a user's liquidity evenly over `n` bins per side (used by `seed`).
    pub fn add_uniform(
        &mut self, owner: &str, token: u128, quote: u128, n: i32, locked_until_ns: u64, now_ns: u64,
    ) -> Result<Vec<u64>, Error> {
        assert!(n > 0, "need at least one bin per side");
        let (t_each, q_each) = (token / n as u128, quote / n as u128);
        let center = self.active;
        let mut ids = Vec::new();
        for i in 1..=n {
            if t_each > 0 {
                ids.push(self.add_liquidity(owner, center + i, t_each, 0, locked_until_ns, now_ns)?);
            }
            if q_each > 0 {
                ids.push(self.add_liquidity(owner, center - i, 0, q_each, locked_until_ns, now_ns)?);
            }
        }
        Ok(ids)
    }
}
