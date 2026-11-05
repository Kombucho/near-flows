//! One-way BUY liquidity, the mirror of tp.rs ("the locked drawer", buy side).
//!
//! A bid sits below the price holding quote. Sellers fill it; the coins they
//! sell go to the bidder's drawer and never re-enter the bin, so a later pump
//! cannot sell them back. The bidder also earns the maker fee slice (in coins).
//!
//! WHEEL: a bid created with a `rearm` preset can be "re-armed" by anyone
//! (the keeper) once it has bought coins: the coins become one-way SELL
//! liquidity above the bid's price. Funds never leave the market and stay owned
//! by the bidder, which is why anyone may trigger it.

use math::acc::{accrue, owed};
use math::price::bins_for_multiple;
use math::wide::mul_div_down;
use math::BPS;

use crate::book::Position;
use crate::{Book, Error, Rung};

/// Wheel exit presets: (multiple of the bid's fill price in bps, share in bps).
pub const WHEEL_EXITS: [&[Rung]; 3] = [
    &[],                                  // 0: no rearm (plain one-way buy)
    &[(12_500, 5_000), (15_000, 5_000)],  // 1: sell half at 1.25x, half at 1.5x of the fill price
    &[(11_100, 5_000), (13_300, 5_000)],  // 2: tighter: 1.11x and 1.33x
];

/// A wheel buy uses at most this many dips, none deeper than MIN_DIP (0.5x).
pub const MAX_DIPS: usize = 4;
pub const MIN_DIP: u32 = 5_000;
/// The default wheel buy: half at 0.95x, half at 0.9x, re-armed with preset 2.
pub const DEFAULT_DIPS: [(u32, u16); 2] = [(9_500, 5_000), (9_000, 5_000)];

impl Book {
    /// Place a one-way bid of `quote` at `bin` (at or below the price).
    pub fn place_bid(
        &mut self, owner: &str, bin: i32, quote: u128, rearm: u8, locked_until_ns: u64, now_ns: u64,
    ) -> Result<u64, Error> {
        if quote == 0 {
            return Err(Error::ZeroAmount);
        }
        if bin > self.active || rearm as usize >= WHEEL_EXITS.len() {
            return Err(Error::WrongSide);
        }
        let _ = now_ns;
        let round = self.bins.get(&bin).map(|b| b.bid_round).unwrap_or(0);
        if let Some(&id) = self.bid_index.get(&(owner.to_string(), bin, round)) {
            let same = self.positions.get(&id).map_or(false, |p| p.locked_until_ns == locked_until_ns && p.rearm == rearm);
            if same {
                return self.top_up_bid(id, quote);
            }
        }
        let (cq, ct) = (self.creator.acc_q, self.creator.acc_t);
        let b = self.bin_mut(bin);
        let minted = if b.bid_shares == 0 { quote } else { mul_div_down(quote, b.bid_shares, b.bid_quote) };
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        b.bid_quote += quote;
        b.bid_shares += minted;
        let pos = Position {
            owner: owner.to_string(), bin, shares: minted, snap_quote: b.bid_acc, snap_token: 0,
            locked_until_ns, creator_weight: 0, snap_cq: cq, snap_ct: ct,
            creator_owed_q: 0, creator_owed_t: 0, take_profit: false, tp_round: b.bid_round, tp_owed: 0,
            bid: true, rearm,
        };
        let round = pos.tp_round;
        let id = self.put_position(pos);
        self.bid_index.insert((owner.to_string(), bin, round), id);
        Ok(id)
    }

    fn top_up_bid(&mut self, id: u64, quote: u128) -> Result<u64, Error> {
        let p = &self.positions[&id];
        let earned = self.bid_earned(p);
        let b = self.bins.get_mut(&p.bin).expect("bin exists");
        let minted = mul_div_down(quote, b.bid_shares, b.bid_quote);
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        b.bid_quote += quote;
        b.bid_shares += minted;
        let acc = b.bid_acc;
        let p = self.positions.get_mut(&id).expect("exists");
        p.tp_owed = earned; // earned already includes the old owed: set, don't add
        p.snap_quote = acc;
        p.shares += minted;
        Ok(id)
    }

    pub(crate) fn bid_acc_for(&self, p: &Position) -> u128 {
        let b = &self.bins[&p.bin];
        if p.tp_round == b.bid_round { b.bid_acc } else { self.bid_closed[&(p.bin, p.tp_round)] }
    }

    /// Coins a bid has bought (plus maker fees) and not yet claimed.
    pub(crate) fn bid_earned(&self, p: &Position) -> u128 {
        p.tp_owed + owed(p.shares, self.bid_acc_for(p), p.snap_quote)
    }

    /// Quote of a bid still waiting to buy.
    pub(crate) fn bid_remaining(&self, p: &Position) -> u128 {
        let b = &self.bins[&p.bin];
        if p.tp_round != b.bid_round || b.bid_shares == 0 {
            return 0;
        }
        mul_div_down(b.bid_quote, p.shares, b.bid_shares)
    }

    /// Record sellers taking `quote_out` from the bid layer at `bin`, paying `coins`.
    pub(crate) fn bid_fill(&mut self, bin: i32, quote_out: u128, coins: u128) {
        let b = self.bins.get_mut(&bin).expect("filled bin exists");
        b.bid_acc = accrue(b.bid_acc, coins, b.bid_shares);
        b.bid_quote -= quote_out;
        if b.bid_quote == 0 {
            self.bid_closed.insert((bin, b.bid_round), b.bid_acc);
            b.bid_round += 1;
            b.bid_shares = 0;
        }
    }

    /// Cancel / close a bid: returns (unspent quote, bought coins).
    pub(crate) fn bid_close(&mut self, id: u64) -> (u128, u128) {
        let p = self.take_position(id).expect("exists");
        self.bid_index.remove(&(p.owner.clone(), p.bin, p.tp_round));
        let (quote, coins) = (self.bid_remaining(&p), self.bid_earned(&p));
        let b = self.bins.get_mut(&p.bin).expect("bin exists");
        if p.tp_round == b.bid_round {
            b.bid_quote -= quote;
            b.bid_shares -= p.shares;
            if b.bid_shares == 0 && b.bid_quote > 0 {
                self.pots.pot_quote += b.bid_quote; // owner-less rounding dust
                b.bid_quote = 0;
            }
        }
        (quote, coins)
    }

    /// WHEEL: turn a bid's bought coins into one-way SELL liquidity above the
    /// bid's own price, at its preset multiples. Anyone may call it: the coins
    /// stay in the market and stay owned by the bidder. Returns new position ids.
    pub fn rearm(&mut self, id: u64, now_ns: u64) -> Result<Vec<u64>, Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        if !p.bid || p.rearm == 0 {
            return Err(Error::NoPosition);
        }
        let (owner, bin, preset) = (p.owner.clone(), p.bin, WHEEL_EXITS[p.rearm as usize]);
        let coins = self.bid_earned(p);
        if coins == 0 {
            return Ok(vec![]);
        }
        // Take the coins out of the bid's drawer (they stay inside the market).
        let acc = self.bid_acc_for(&self.positions[&id]);
        let p = self.positions.get_mut(&id).expect("exists");
        p.tp_owed = 0;
        p.snap_quote = acc;
        let mut ids = Vec::new();
        let mut placed = 0u128;
        for (i, &(multiple, share)) in preset.iter().enumerate() {
            let amount = if i + 1 == preset.len() { coins - placed } else { coins * share as u128 / BPS };
            if amount == 0 { continue; }
            let target = bin + bins_for_multiple(self.cfg.step_bps, multiple);
            let at = target.max(self.active + 1); // already above target: sell at the next bin up
            ids.push(self.place_take_profit(&owner, at, amount, 0, now_ns)?);
            placed += amount;
        }
        Ok(ids)
    }

    /// Bids among ids from_id..from_id+scan that have bought coins and are
    /// set to rearm (for keepers, who page through ids up to `next_id`).
    pub fn rearmable(&self, from_id: u64, scan: u64) -> Vec<u64> {
        (from_id..from_id.saturating_add(scan).min(self.next_id))
            .filter(|id| self.positions.get(id).map_or(false, |p| p.bid && p.rearm > 0 && self.bid_earned(p) > 0))
            .collect()
    }

    /// WHEEL BUY: put `quote` as one-way bids at `dips` below the current price
    /// (dip = (multiple below, e.g. 9_000 = 0.9x, share in bps)), set to rearm.
    pub fn place_wheel(&mut self, owner: &str, quote: u128, dips: &[(u32, u16)], rearm: u8, now_ns: u64) -> Result<Vec<u64>, Error> {
        // Check everything first: a half-placed wheel must never happen.
        let shares: u32 = dips.iter().map(|&(_, s)| s as u32).sum();
        let bad = dips.is_empty() || dips.len() > MAX_DIPS || shares != BPS as u32
            || dips.iter().any(|&(below, _)| below < MIN_DIP || below >= BPS as u32);
        if bad {
            return Err(Error::BadRange);
        }
        if quote == 0 {
            return Err(Error::ZeroAmount);
        }
        let mut ids = Vec::new();
        let mut placed = 0u128;
        for (i, &(below, share)) in dips.iter().enumerate() {
            let amount = if i + 1 == dips.len() { quote - placed } else { quote * share as u128 / BPS };
            if amount == 0 { continue; }
            // bins below for a price multiple `below` (< 1.0): same distance as 1/below up.
            let up = (BPS as u64 * BPS as u64 / below as u64) as u32;
            let bin = (self.active - bins_for_multiple(self.cfg.step_bps, up)).min(self.active - 1); // strictly below
            ids.push(self.place_bid(owner, bin, amount, rearm, 0, now_ns)?);
            placed += amount;
        }
        Ok(ids)
    }
}
