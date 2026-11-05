//! Solvency audit: everything the market owes anyone, right now.
//! The contract's real balances must always be at least this. Tests and the
//! simulator compare the two; any shortfall means value came from nothing.
//!
//! On chain the positions are paged (a view can't read thousands at once):
//! obligations = base + the sum of `obligations_of` over every id page.

use crate::Book;

impl Book {
    /// Total obligations: (quote, token). Reads every position.
    pub fn obligations(&self) -> (u128, u128) {
        let (q, t) = self.obligations_base();
        let (pq, pt) = self.obligations_of(0, self.next_id);
        (q + pq, t + pt)
    }

    /// Everything except position fees: pots, bin reserves, waiting one-way
    /// orders, stakes and dividends.
    pub fn obligations_base(&self) -> (u128, u128) {
        let mut q = self.pots.pot_quote + self.pots.creator_quote + self.pots.protocol_quote + self.burn_quote;
        let mut t = self.pots.pot_token + self.pots.creator_token + self.pots.protocol_token;
        for id in &self.bin_ids {
            let b = &self.bins[id];
            q += b.quote + b.bid_quote; // waiting one-way bids belong to their owners
            t += b.token + b.tp_token; // unsold take-profit tokens belong to their owners
        }
        let v = &self.vault;
        t += v.total_stake + v.total_pending + v.div_open_t;
        q += v.div_open_q;
        (q, t)
    }

    /// Fees and proceeds claimable by positions with ids in from_id..from_id+count.
    pub fn obligations_of(&self, from_id: u64, count: u64) -> (u128, u128) {
        let (mut q, mut t) = (0, 0);
        for id in from_id..from_id.saturating_add(count).min(self.next_id) {
            if let Ok((fq, ft)) = self.claimable(id) {
                q += fq;
                t += ft;
            }
        }
        (q, t)
    }
}
