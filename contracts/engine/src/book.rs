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
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct Bin {
    pub token: u128,
    pub quote: u128,
    /// Total LP shares in this bin. Shares = claim on the bin's reserves.
    pub shares: u128,
    /// Fee-per-share counters (see math::acc), one per currency.
    pub acc_quote: u128,
    pub acc_token: u128,
    /// Take-profit layer: one-way sell orders. Buyers take these tokens first;
    /// the quote they pay goes to the order owners' "drawer" (`tp_acc`), never
    /// back into the bin, so sellers can never undo a filled take-profit.
    pub tp_token: u128,  // tokens still waiting to be sold
    pub tp_shares: u128, // shares of the current round of orders
    pub tp_acc: u128,    // quote earned per share (fill proceeds + maker fee slice)
    /// Increases each time the layer sells out completely. Orders placed
    /// after that start a new round, so they never share earlier proceeds.
    pub tp_round: u32,
    /// One-way BUY layer (mirror of the take-profit layer): quote waiting to
    /// buy at this price. Sellers fill it first; the coins they hand over go to
    /// the bidders' drawer (`bid_acc`), never back into the bin, so a later
    /// pump can't sell them back.
    pub bid_quote: u128,
    pub bid_shares: u128,
    pub bid_acc: u128, // coins earned per share (fills + maker fee slice)
    pub bid_round: u32,
}

/// Fixed at creation. None of this can change afterwards.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
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
    /// true = a coin launched on this protocol: full mechanics (stake, vote,
    /// burn, creator slice, launch window). false = an adopted existing coin:
    /// take-profit ladders and liquidity only, fees 90% LPs / 10% deepen.
    pub native: bool,
    /// Dynamic fee: extra bps per (bins of movement)^2. 0 = fixed fee.
    /// Fee for a bin = fee_bps + dyn_k * (recent volatility + bins moved so far)^2,
    /// capped at max_fee_bps. Volatile moments cost more, which pays LPs for
    /// the extra risk and makes picking off stale prices expensive.
    pub dyn_k: u16,
    pub max_fee_bps: u16,
    /// Protocol slice: at most this share of every fee (1_000 = 10%) goes to
    /// the protocol, and ONLY once the coin is busy. Off below
    /// `protocol_threshold` of trailing daily quote volume, phasing in linearly
    /// to the full share at 2x the threshold, and off again if traffic fades.
    pub protocol_bps: u16,
    pub protocol_threshold: u128,
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
            native: true,
            dyn_k: 0,
            max_fee_bps: fee_bps,
            protocol_bps: 0,
            protocol_threshold: 0,
        }
    }

    /// An existing coin brought into the layer. No pot vote, no creator slice,
    /// no launch window: 90% of fees to LPs, 10% to protocol-owned depth.
    pub fn adopted(base_price: u128, fee_bps: u16) -> Self {
        Config {
            shares: FeeShares { pot_bps: 1_000, creator_bps: 0 },
            launch_window_ns: 0,
            native: false,
            ..Config::standard(base_price, fee_bps)
        }
    }
}

/// One LP's liquidity in one bin.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
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
    /// true = a one-way take-profit order (lives in the bin's tp layer).
    /// For these, `snap_quote` is the snapshot of `tp_acc`.
    pub take_profit: bool,
    pub tp_round: u32,
    /// One-way proceeds settled when the order was topped up, not yet claimed
    /// (quote for take-profits, coins for bids).
    pub tp_owed: u128,
    /// true = a one-way BID (buy liquidity below the price). For these,
    /// `snap_quote` is the snapshot of `bid_acc` and `tp_round` is the bid round.
    pub bid: bool,
    /// Wheel: once this bid has bought coins, `rearm` places them as one-way
    /// sell liquidity at this preset (index into WHEEL_EXITS; 0 = none).
    pub rearm: u8,
}

/// Fee slices that do not go to LPs, waiting for the next crank.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct Pots {
    pub pot_quote: u128,
    pub pot_token: u128,
    pub creator_quote: u128,
    pub creator_token: u128,
    /// The protocol slice, waiting for the owner to withdraw it.
    pub protocol_quote: u128,
    pub protocol_token: u128,
}

/// The creator slice is shared by locked liquidity, pro-rata to weight.
#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct CreatorPool {
    pub total_weight: u128,
    pub acc_q: u128,
    pub acc_t: u128,
    pub locked: BTreeSet<u64>, // position ids that currently carry weight
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
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
    /// Final `tp_acc` of every take-profit round that sold out: (bin, round) -> acc.
    pub tp_closed: BTreeMap<(i32, u32), u128>,
    /// Recent volatility in bins crossed. Grows with every swap's movement and
    /// halves every 10 minutes of real time, so fees fall back soon after a
    /// burst (Meteora-style). `vol_updated_ns` is when it was last decayed.
    pub vol_bins: u32,
    pub vol_updated_ns: u64,
    /// Quote volume traded since the last crank, and a smoothed daily volume
    /// (updated each crank). The protocol slice is measured against the latter.
    pub hour_volume: u128,
    pub daily_volume: u128,
    /// (owner, bin, round) -> that owner's open take-profit there, so repeat
    /// orders top it up instead of creating new positions (bounded storage).
    pub tp_index: BTreeMap<(String, i32, u32), u64>,
    /// Same as tp_closed / tp_index, for the one-way bid layer.
    pub bid_closed: BTreeMap<(i32, u32), u128>,
    pub bid_index: BTreeMap<(String, i32, u32), u64>,
}

impl Book {
    pub fn new(cfg: Config) -> Self {
        assert!(cfg.fee_bps < 10_000, "fee must be below 100%");
        assert!(cfg.max_fee_bps >= cfg.fee_bps && cfg.max_fee_bps < 10_000, "bad max fee");
        assert!(cfg.base_price > 0, "price must be positive");
        Book {
            cfg, active: 0, bins: BTreeMap::new(), positions: BTreeMap::new(), next_id: 0,
            pots: Pots::default(), creator: CreatorPool::default(), vault: Vault::default(),
            protocol_pos: BTreeMap::new(), burn_quote: 0, burned: 0, last_crank_ns: 0, launched_ns: 0,
            ref_bin: 0, tp_closed: BTreeMap::new(), vol_bins: 0, vol_updated_ns: 0,
            hour_volume: 0, daily_volume: 0, tp_index: BTreeMap::new(),
            bid_closed: BTreeMap::new(), bid_index: BTreeMap::new(),
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
        let eligible = self.cfg.native && owner != PROTOCOL
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
            creator_owed_q: 0, creator_owed_t: 0, take_profit: false, tp_round: 0, tp_owed: 0, bid: false, rearm: 0,
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
