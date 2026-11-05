//! The crank: a public, rate-limited call anyone can make (hourly by default).
//! It is the only place the community pot is spent, always by the same rules:
//!   1. expired locks stop earning the creator slice
//!   2. the protocol's own LP fees go into the pot (they compound for stakers)
//!   3. the creator slice goes to locked liquidity (or the pot if none)
//!   4. the pot splits by the stakers' votes into dividend / burn / deepen
//!   5. burn: token is burned directly, quote buys the coin back and burns it
//!   6. deepen: the protocol adds liquidity around the current price
//! No owner input, no price targets: the same inputs always do the same thing.

use math::fees::{split_pot, PotVotes};

const ALL_DEEPEN: PotVotes = PotVotes { dividend: 0, burn: 0, deepen: 1 };

use crate::{Book, Error, Side};

/// How far (in bins, 1% each) the price may sit from its slow reference
/// before buyback and deepen pause. 10 bins = about 10%.
pub const MAX_DRIFT_BINS: i32 = 10;

/// How far the reference may move per crank (hourly): 2 bins, about 2%.
pub const MAX_FOLLOW_BINS: i32 = 2;

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
        self.touch(now_ns); // volatility memory fades with time (10-minute half-life)
        // Smoothed daily volume: move 1/24 of the way toward "this hour x 24".
        let target = std::mem::take(&mut self.hour_volume).saturating_mul(24);
        self.daily_volume = if target >= self.daily_volume {
            self.daily_volume + (target - self.daily_volume) / 24
        } else {
            self.daily_volume - (self.daily_volume - target) / 24
        };
        self.expire_locks(now_ns);
        self.collect_protocol_fees();
        self.distribute_creator();
        let (q, t) = (std::mem::take(&mut self.pots.pot_quote), std::mem::take(&mut self.pots.pot_token));
        // Adopted coins have no stakers: their whole pot deepens the market.
        let votes = if self.cfg.native { self.vault.votes } else { ALL_DEEPEN };
        let sq = split_pot(q, votes);
        let st = split_pot(t, votes);
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
        self.follow_price();
        if self.price_is_calm() {
            r.burned += self.buy_back();
            self.deepen(sq.deepen, st.deepen);
            (r.deepen_q, r.deepen_t) = (sq.deepen, st.deepen);
        } else {
            // Price far from its slow reference: maybe manipulated. Wait.
            self.pots.pot_quote += sq.deepen;
            self.pots.pot_token += st.deepen;
        }
        Ok(r)
    }

    /// Move the reference toward the price, but only as far as the price has
    /// sat on that side at both this crank and the last one (a spike pushed
    /// right before the crank moves nothing), and at most MAX_FOLLOW_BINS.
    fn follow_price(&mut self) {
        let now = self.active - self.ref_bin;
        let before = self.prev_crank_bin - self.ref_bin;
        self.prev_crank_bin = self.active;
        let gap = if now.signum() != before.signum() { 0 } else if now.abs() < before.abs() { now } else { before };
        self.ref_bin += gap.clamp(-MAX_FOLLOW_BINS, MAX_FOLLOW_BINS);
    }

    fn price_is_calm(&self) -> bool {
        (self.active - self.ref_bin).abs() <= MAX_DRIFT_BINS
    }

    /// The protocol's LP fees are not paid to anyone: they refill the pot.
    /// Only bins traded since the last crank can have earned anything.
    fn collect_protocol_fees(&mut self) {
        let bins = std::mem::take(&mut self.traded_bins);
        let ids: Vec<u64> = bins.iter().filter_map(|b| self.protocol_pos.get(b).copied()).collect();
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
        let cap = self.ref_bin + MAX_DRIFT_BINS; // never pay more than ~10% over reference
        match self.swap_within(Side::Buy, self.burn_quote, 0, Some(cap)) {
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
            // An amount too small to mint even one share (tiny pot, big bin) must not
            // stop the crank: it simply waits in the pot. A panic here would block
            // every future crank, i.e. no burns and no dividends ever again.
            if q_each > 0 && self.add_protocol(center - i, 0, q_each).is_err() {
                self.pots.pot_quote += q_each;
            }
            if t_each > 0 && self.add_protocol(center + i, t_each, 0).is_err() {
                self.pots.pot_token += t_each;
            }
        }
    }
}
