//! Solvency audit: everything the market owes anyone, right now.
//! The contract's real balances must always be at least this. Tests and the
//! simulator compare the two; any shortfall means value came from nothing.

use math::acc::owed;

use crate::Book;

impl Book {
    /// Total obligations: (quote, token).
    pub fn obligations(&self) -> (u128, u128) {
        let mut q = self.pots.pot_quote + self.pots.creator_quote + self.pots.protocol_quote + self.burn_quote;
        let mut t = self.pots.pot_token + self.pots.creator_token + self.pots.protocol_token;
        for b in self.bins.values() {
            q += b.quote + b.bid_quote; // waiting one-way bids belong to their owners
            t += b.token + b.tp_token; // unsold take-profit tokens belong to their owners
        }
        for id in self.positions.keys() {
            let (fq, ft) = self.claimable(*id).expect("listed position exists");
            q += fq;
            t += ft;
        }
        let v = &self.vault;
        for s in v.stakers.values() {
            t += s.stake + s.pending + s.owed_t + owed(s.stake, v.acc_t, s.snap_t);
            q += s.owed_q + owed(s.stake, v.acc_q, s.snap_q);
        }
        (q, t)
    }
}
