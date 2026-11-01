//! One-way take-profit orders ("the locked drawer").
//!
//! A take-profit sits in a bin's tp layer. Buyers take its tokens; the quote
//! they pay is credited to the order owners and never re-enters the bin, so a
//! later dump cannot buy the tokens back for them.
//!
//! Many orders at one bin share fills pro-rata with the same counter trick as
//! fees: if buyers take 40% of the tokens waiting, every order is 40% filled.

use math::acc::{accrue, owed};
use math::wide::mul_div_down;

use crate::book::Position;
use crate::{Book, Error};

impl Book {
    /// Place a one-way sell order of `tokens` at `bin` (at or above the price).
    pub fn place_take_profit(
        &mut self, owner: &str, bin: i32, tokens: u128, locked_until_ns: u64, now_ns: u64,
    ) -> Result<u64, Error> {
        if tokens == 0 {
            return Err(Error::ZeroAmount);
        }
        if bin < self.active {
            return Err(Error::WrongSide);
        }
        let value = self.value(bin, tokens, 0);
        let eligible = self.cfg.native && locked_until_ns >= now_ns.saturating_add(self.cfg.min_creator_lock_ns);
        let weight = if eligible { value } else { 0 };
        let b = self.bins.entry(bin).or_default();
        // First order of a round: 1 share per token. Later: pro-rata to what is left.
        let minted = if b.tp_shares == 0 { tokens } else { mul_div_down(tokens, b.tp_shares, b.tp_token) };
        if minted == 0 {
            return Err(Error::ZeroAmount);
        }
        b.tp_token += tokens;
        b.tp_shares += minted;
        let pos = Position {
            owner: owner.to_string(), bin, shares: minted, snap_quote: b.tp_acc, snap_token: 0,
            locked_until_ns, creator_weight: weight, snap_cq: self.creator.acc_q, snap_ct: self.creator.acc_t,
            creator_owed_q: 0, creator_owed_t: 0, take_profit: true, tp_round: b.tp_round,
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

    /// The tp counter value that applies to a position: the live one if its
    /// round is still open, or the frozen final value if that round sold out.
    pub(crate) fn tp_acc_for(&self, p: &Position) -> u128 {
        let b = &self.bins[&p.bin];
        if p.tp_round == b.tp_round { b.tp_acc } else { self.tp_closed[&(p.bin, p.tp_round)] }
    }

    /// Quote a take-profit order has earned and not yet claimed.
    pub(crate) fn tp_earned(&self, p: &Position) -> u128 {
        owed(p.shares, self.tp_acc_for(p), p.snap_quote)
    }

    /// Tokens of a take-profit order still waiting to be sold.
    pub(crate) fn tp_remaining(&self, p: &Position) -> u128 {
        let b = &self.bins[&p.bin];
        if p.tp_round != b.tp_round || b.tp_shares == 0 {
            return 0; // its round sold out
        }
        mul_div_down(b.tp_token, p.shares, b.tp_shares)
    }

    /// Record a fill of `out` tp tokens at `bin` that earned `quote` for the owners.
    pub(crate) fn tp_fill(&mut self, bin: i32, out: u128, quote: u128) {
        let b = self.bins.get_mut(&bin).expect("filled bin exists");
        b.tp_acc = accrue(b.tp_acc, quote, b.tp_shares);
        b.tp_token -= out;
        if b.tp_token == 0 {
            // Sold out: freeze this round's counter and start a fresh round.
            self.tp_closed.insert((bin, b.tp_round), b.tp_acc);
            b.tp_round += 1;
            b.tp_shares = 0;
        }
    }

    /// Cancel / close a take-profit: returns (unsold tokens, earned quote).
    pub(crate) fn tp_close(&mut self, id: u64) -> (u128, u128) {
        let p = self.positions.remove(&id).expect("exists");
        let (token, quote) = (self.tp_remaining(&p), self.tp_earned(&p));
        let b = self.bins.get_mut(&p.bin).expect("bin exists");
        if p.tp_round == b.tp_round {
            b.tp_token -= token;
            b.tp_shares -= p.shares;
            if b.tp_shares == 0 && b.tp_token > 0 {
                // Rounding dust with no owner left: it joins the community pot.
                self.pots.pot_token += b.tp_token;
                b.tp_token = 0;
            }
        }
        (token, quote)
    }
}
