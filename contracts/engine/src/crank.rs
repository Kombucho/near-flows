//! The crank: a public, rate-limited call anyone can make (hourly by default).
//! It is the only place the community pot is spent, always by the same rules:
//!   1. expired locks stop earning the creator slice
//!   2. the protocol's own LP fees go into the pot (they compound for stakers)
//!   3. the creator slice goes to locked liquidity (or the pot if none)
//!   4. the pot splits by the stakers' votes into dividend / burn / deepen
//!   5. burn: token is burned directly, quote buys the coin back and burns it
//!   6. deepen: the protocol adds liquidity around the current price
//! No owner input, no price targets: the same inputs always do the same thing.

use math::fees::split_pot;

use crate::{Book, Error, Side};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CrankReport {
    pub dividend_q: u128,
    pub dividend_t: u128,
    pub burned: u128,
    pub deepen_q: u128,
    pub deepen_t: u128,
}

impl Book {
    pub fn crank(&mut self, now_ns: u64) -> Result<CrankReport, Error> {
        let next = self.last_crank_ns.saturating_add(self.cfg.crank_interval_ns);
        if self.last_crank_ns != 0 && now_ns < next {
            return Err(Error::TooSoon { next_ns: next });
        }
        self.last_crank_ns = now_ns;
        self.expire_locks(now_ns);
        self.collect_protocol_fees();
        self.distribute_creator();
        let (q, t) = (std::mem::take(&mut self.pots.pot_quote), std::mem::take(&mut self.pots.pot_token));
        let sq = split_pot(q, self.vault.votes);
        let st = split_pot(t, self.vault.votes);
        let mut r = CrankReport::default();
        // Dividends. With no stakers there is nobody to pay, so it is burned instead.
        if self.vault.distribute(sq.dividend, st.dividend) {
            (r.dividend_q, r.dividend_t) = (sq.dividend, st.dividend);
        } else {
            self.burn_quote += sq.dividend;
            self.burned += st.dividend;
            r.burned += st.dividend;
        }
        self.burned += st.burn;
        r.burned += st.burn;
        self.burn_quote += sq.burn;
        r.burned += self.buy_back();
        self.deepen(sq.deepen, st.deepen);
        (r.deepen_q, r.deepen_t) = (sq.deepen, st.deepen);
        Ok(r)
    }

    /// The protocol's LP fees are not paid to anyone: they refill the pot.
    fn collect_protocol_fees(&mut self) {
        let ids: Vec<u64> = self.protocol_pos.values().copied().collect();
        for id in ids {
            let (q, t) = self.claim(id, crate::book::PROTOCOL).expect("protocol owns it");
            self.pots.pot_quote += q;
            self.pots.pot_token += t;
        }
    }

    /// Spend the buyback quote on the coin and burn what it buys. Returns tokens burned.
    /// If there is nothing to buy right now, the quote waits for the next crank.
    fn buy_back(&mut self) -> u128 {
        if self.burn_quote == 0 {
            return 0;
        }
        match self.swap(Side::Buy, self.burn_quote, 0) {
            Ok(r) => {
                self.burn_quote = r.refund;
                self.burned += r.out;
                r.out
            }
            Err(_) => 0,
        }
    }

    /// Add protocol liquidity: quote as bids below the price, tokens as asks above.
    fn deepen(&mut self, q: u128, t: u128) {
        let n = self.cfg.deepen_bins;
        let (q_each, t_each) = (q / n as u128, t / n as u128);
        // Whatever does not divide evenly goes back to the pot for next time.
        self.pots.pot_quote += q - q_each * n as u128;
        self.pots.pot_token += t - t_each * n as u128;
        let center = self.active;
        for i in 1..=n {
            if q_each > 0 {
                self.add_protocol(center - i, 0, q_each).expect("bids below price");
            }
            if t_each > 0 {
                self.add_protocol(center + i, t_each, 0).expect("asks above price");
            }
        }
    }
}
