//! The rules, with no blockchain in them: what a buy gets, who leads an
//! auction, what the winner pays. The contract (lib.rs) moves the real tokens.
//!
//! AUCTIONS follow Auction Anything (visualizevalue/auctionanything):
//!   - proxy bidding: you lock your MAXIMUM; the public price only rises to
//!     one step above the runner-up's maximum, so you usually pay less;
//!   - steps of 10%; equal maxima keep the earlier bidder;
//!   - a bid in the last 10 minutes moves the close to 10 minutes after it;
//!   - one live offer per bidder: bidding again raises your maximum.
//! One difference: on a public chain your locked maximum can be read by
//! anyone who looks. The app hides it, but it is not secret.
//!
//! LISTINGS: a fixed price for a fixed amount. Buyers may take part of it at
//! the same price per unit; rounding always favours the seller by < 1 unit.

use math::wide::{mul_div_down, mul_div_up};
use near_sdk::near;

pub const STEP_BPS: u128 = 1_000; // 10%
pub const SNIPE_NS: u64 = 10 * 60 * 1_000_000_000; // 10 minutes
pub const MIN_DURATION_NS: u64 = 60 * 60 * 1_000_000_000; // 1 hour
pub const MAX_DURATION_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000; // 30 days

#[derive(Debug, PartialEq, Eq)]
pub enum RuleError {
    ZeroAmount,
    TooSmall,               // pays for less than one unit
    TooLow { min: u128 },   // bid below the next allowed bid
    Closed,
    NotClosed { ends_ns: u64 },
    AlreadySettled,
    HasBids,
    BadDuration,
}

/// What a listing still has for sale: `left` of the `amount` listed, at
/// `ask` (total price for the whole `amount`).
#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Price {
    pub amount: u128,
    pub ask: u128,
}

/// A buy of a listing: units received, payment used (the rest is refunded).
#[derive(Debug, PartialEq, Eq)]
pub struct Fill {
    pub out: u128,
    pub used: u128,
}

/// Buy from a listing with `paid`. `left` = units still for sale.
/// `whole_only`: indivisible items (an NFT, an idea) must be bought whole.
pub fn fill(p: Price, left: u128, paid: u128, whole_only: bool) -> Result<Fill, RuleError> {
    if paid == 0 || left == 0 {
        return Err(RuleError::ZeroAmount);
    }
    if whole_only {
        let cost = mul_div_up(left, p.ask, p.amount);
        return if paid >= cost { Ok(Fill { out: left, used: cost }) } else { Err(RuleError::TooLow { min: cost }) };
    }
    let out = mul_div_down(paid, p.amount, p.ask);
    if out == 0 {
        return Err(RuleError::TooSmall);
    }
    if out >= left {
        // The rest of the listing: pay exactly its price (rounded up), refund the excess.
        return Ok(Fill { out: left, used: mul_div_up(left, p.ask, p.amount).min(paid) });
    }
    Ok(Fill { out, used: paid })
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bid {
    pub who: String,
    pub max: u128, // locked in the contract
    pub at_ns: u64,
}

/// An auction's live state.
#[near(serializers = [borsh])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Auction {
    pub opening: u128, // first bid must be at least this
    pub reserve: u128, // below this at the close, nothing sells (0 = none)
    pub ends_ns: u64,
    pub leader: Option<Bid>,
    pub runner_up_max: u128,
    pub price: u128, // public price: what the leader pays if it closed now
    pub bids: u32,
    pub settled: bool,
}

/// What a bid did. `refund`: someone's locked maximum to send back now
/// (the outbid leader, or the new bidder if they did not take the lead).
#[derive(Debug, PartialEq, Eq)]
pub struct BidResult {
    pub leads: bool,
    pub refund: Option<(String, u128)>,
    pub price: u128,
    pub extended: bool,
}

/// The close: who gets the item, and the money flows.
#[derive(Debug, PartialEq, Eq)]
pub enum Close {
    /// Sold: `winner` pays `price` to the seller and gets `change` back.
    Sold { winner: String, price: u128, change: u128 },
    /// Not sold (no bids, or reserve not met): item back to the seller,
    /// and the leader's maximum (if any) back to them.
    Unsold { refund: Option<(String, u128)> },
}

/// The next allowed public bid after `price`: +10%, rounded up, at least +1.
pub fn next_bid(price: u128) -> u128 {
    price + mul_div_up(price, STEP_BPS, 10_000).max(1)
}

impl Auction {
    pub fn new(opening: u128, reserve: u128, now_ns: u64, duration_ns: u64) -> Result<Auction, RuleError> {
        if opening == 0 {
            return Err(RuleError::ZeroAmount);
        }
        if !(MIN_DURATION_NS..=MAX_DURATION_NS).contains(&duration_ns) {
            return Err(RuleError::BadDuration);
        }
        Ok(Auction { opening, reserve, ends_ns: now_ns + duration_ns, leader: None, runner_up_max: 0, price: 0, bids: 0, settled: false })
    }

    /// The least a NEW bidder must lock right now.
    pub fn min_bid(&self) -> u128 {
        if self.leader.is_some() { next_bid(self.price) } else { self.opening }
    }

    /// `who` locks `amount` more. The current leader adding to their bid only
    /// raises their maximum; anyone else must lock at least `min_bid()`.
    pub fn bid(&mut self, who: &str, amount: u128, now_ns: u64) -> Result<BidResult, RuleError> {
        if self.settled || now_ns >= self.ends_ns {
            return Err(RuleError::Closed);
        }
        if amount == 0 {
            return Err(RuleError::ZeroAmount);
        }
        let mut refund = None;
        let leads;
        match self.leader.as_mut() {
            Some(l) if l.who == who => {
                l.max += amount; // one live offer per bidder: raise it
                leads = true;
            }
            _ => {
                let min = self.min_bid();
                if amount < min {
                    return Err(RuleError::TooLow { min });
                }
                let new = Bid { who: who.to_string(), max: amount, at_ns: now_ns };
                match self.leader.take() {
                    None => {
                        self.price = self.opening;
                        self.leader = Some(new);
                        leads = true;
                    }
                    Some(old) if new.max > old.max => {
                        // New leader. The old one is outbid: their maximum goes back.
                        self.runner_up_max = self.runner_up_max.max(old.max);
                        self.price = new.max.min(self.price.max(next_bid(self.runner_up_max)));
                        refund = Some((old.who, old.max));
                        self.leader = Some(new);
                        leads = true;
                    }
                    Some(old) => {
                        // Not enough to lead (ties keep the earlier bid), but it
                        // pushes the leader's public price up to just above it.
                        self.runner_up_max = self.runner_up_max.max(new.max);
                        self.price = old.max.min(self.price.max(next_bid(new.max)));
                        refund = Some((new.who, new.max));
                        self.leader = Some(old);
                        leads = false;
                    }
                }
            }
        }
        // Reaching the reserve: the price jumps to it if the leader's maximum covers it.
        let lmax = self.leader.as_ref().map_or(0, |l| l.max);
        self.price = self.price.max(self.reserve.min(lmax));
        self.bids += 1;
        let extended = now_ns + SNIPE_NS > self.ends_ns;
        if extended {
            self.ends_ns = now_ns + SNIPE_NS;
        }
        Ok(BidResult { leads, refund, price: self.price, extended })
    }

    /// Close the auction (anyone, once it has ended).
    pub fn close(&mut self, now_ns: u64) -> Result<Close, RuleError> {
        if self.settled {
            return Err(RuleError::AlreadySettled);
        }
        if now_ns < self.ends_ns {
            return Err(RuleError::NotClosed { ends_ns: self.ends_ns });
        }
        self.settled = true;
        Ok(match self.leader.clone() {
            Some(l) if l.max >= self.reserve => {
                Close::Sold { winner: l.who, price: self.price, change: l.max - self.price }
            }
            Some(l) => Close::Unsold { refund: Some((l.who, l.max)) },
            None => Close::Unsold { refund: None },
        })
    }

    /// The seller may cancel only before the first bid.
    pub fn cancel(&mut self) -> Result<(), RuleError> {
        if self.leader.is_some() {
            return Err(RuleError::HasBids);
        }
        if self.settled {
            return Err(RuleError::AlreadySettled);
        }
        self.settled = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 60 * 60 * 1_000_000_000;

    fn auction() -> Auction {
        Auction::new(100, 0, 0, 48 * H).unwrap()
    }

    #[test]
    fn a_listing_sells_in_parts_at_the_same_price_per_unit() {
        let p = Price { amount: 1_000, ask: 500 }; // 2 units per 1 paid
        assert_eq!(fill(p, 1_000, 100, false), Ok(Fill { out: 200, used: 100 }));
        assert_eq!(fill(p, 800, 1_000, false), Ok(Fill { out: 800, used: 400 }), "overpay: take the rest, refund 600");
        assert_eq!(fill(Price { amount: 1, ask: 500 }, 1, 499, false), Err(RuleError::TooSmall));
        assert_eq!(fill(Price { amount: 1, ask: 500 }, 1, 499, true), Err(RuleError::TooLow { min: 500 }));
        assert_eq!(fill(Price { amount: 1, ask: 500 }, 1, 600, true), Ok(Fill { out: 1, used: 500 }), "an NFT is bought whole");
    }

    #[test]
    fn rounding_never_lets_a_buyer_pay_less_than_the_price() {
        let p = Price { amount: 3, ask: 10 }; // 3.33 per unit
        let f = fill(p, 3, 1_000, false).unwrap();
        assert_eq!((f.out, f.used), (3, 10));
        let f = fill(p, 1, 1_000, false).unwrap(); // last unit: 3.33 rounds UP to 4
        assert_eq!((f.out, f.used), (1, 4));
    }

    #[test]
    fn the_leader_pays_one_step_above_the_runner_up_not_their_maximum() {
        let mut a = auction();
        assert_eq!(a.bid("alice", 99, H), Err(RuleError::TooLow { min: 100 }));
        let r = a.bid("alice", 1_000, H).unwrap();
        assert!(r.leads && r.refund.is_none());
        assert_eq!(a.price, 100, "alone: pays the opening bid");
        let r = a.bid("bob", 200, 2 * H).unwrap();
        assert!(!r.leads);
        assert_eq!(r.refund, Some(("bob".into(), 200)), "bob is outbid at once and refunded");
        assert_eq!(a.price, 220, "alice's price rises to one step above bob's maximum");
        let r = a.bid("carol", 2_000, 3 * H).unwrap();
        assert!(r.leads);
        assert_eq!(r.refund, Some(("alice".into(), 1_000)), "alice's whole maximum goes back");
        assert_eq!(a.price, 1_100);
        a.ends_ns = 48 * H;
        assert_eq!(a.close(48 * H), Ok(Close::Sold { winner: "carol".into(), price: 1_100, change: 900 }));
    }

    #[test]
    fn a_tie_keeps_the_earlier_bidder_at_their_full_maximum() {
        let mut a = auction();
        a.bid("alice", 500, H).unwrap();
        let r = a.bid("bob", 500, 2 * H).unwrap();
        assert!(!r.leads);
        assert_eq!(a.price, 500, "capped at alice's maximum");
        assert_eq!(a.leader.as_ref().unwrap().who, "alice");
    }

    #[test]
    fn bidding_again_raises_your_own_maximum() {
        let mut a = auction();
        a.bid("alice", 150, H).unwrap();
        let r = a.bid("alice", 50, 2 * H).unwrap();
        assert!(r.leads && r.refund.is_none());
        assert_eq!(a.leader.as_ref().unwrap().max, 200);
        assert_eq!(a.price, 100, "raising your own maximum does not raise your price");
    }

    #[test]
    fn a_bid_in_the_last_ten_minutes_extends_the_close() {
        let mut a = auction();
        let late = 48 * H - SNIPE_NS / 2;
        let r = a.bid("alice", 100, late).unwrap();
        assert!(r.extended);
        assert_eq!(a.ends_ns, late + SNIPE_NS);
        let later = late + SNIPE_NS / 2;
        assert!(a.bid("bob", 200, later).unwrap().extended, "and again for the next late bid");
        assert_eq!(a.ends_ns, later + SNIPE_NS);
        assert_eq!(a.bid("carol", 1_000, a.ends_ns), Err(RuleError::Closed));
    }

    #[test]
    fn the_reserve_is_met_or_nothing_sells() {
        let mut a = Auction::new(100, 500, 0, 48 * H).unwrap();
        a.bid("alice", 300, H).unwrap();
        assert_eq!(a.close(48 * H), Ok(Close::Unsold { refund: Some(("alice".into(), 300)) }));
        let mut a = Auction::new(100, 500, 0, 48 * H).unwrap();
        a.bid("alice", 800, H).unwrap();
        assert_eq!(a.price, 500, "a maximum above the reserve jumps the price to the reserve");
        assert_eq!(a.close(48 * H), Ok(Close::Sold { winner: "alice".into(), price: 500, change: 300 }));
    }

    #[test]
    fn closing_happens_once_and_only_after_the_end() {
        let mut a = auction();
        assert_eq!(a.close(H), Err(RuleError::NotClosed { ends_ns: 48 * H }));
        assert_eq!(a.close(48 * H), Ok(Close::Unsold { refund: None }));
        assert_eq!(a.close(49 * H), Err(RuleError::AlreadySettled));
        let mut b = auction();
        b.bid("alice", 100, H).unwrap();
        assert_eq!(b.cancel(), Err(RuleError::HasBids), "no pulling the item once someone has bid");
    }

    #[test]
    fn the_money_always_adds_up() {
        // Random bids: at every step, locked = leader's max; refunds + locked = everything bid.
        let mut seed = 7u64;
        let mut rnd = || { seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); seed >> 33 };
        for _ in 0..200 {
            let mut a = auction();
            let (mut bid_total, mut refunded) = (0u128, 0u128);
            for t in 0..30u64 {
                let who = ["a", "b", "c", "d"][(rnd() % 4) as usize];
                let amt = a.min_bid() + (rnd() % 500) as u128;
                if let Ok(r) = a.bid(who, amt, t * H) {
                    bid_total += amt;
                    refunded += r.refund.map_or(0, |x| x.1);
                    let l = a.leader.as_ref().unwrap();
                    assert_eq!(bid_total - refunded, l.max, "only the leader's maximum stays locked");
                    assert!(a.price <= l.max, "never pays more than their maximum");
                }
            }
            let ends = a.ends_ns;
            if let Ok(Close::Sold { price, change, .. }) = a.close(ends) {
                assert_eq!(price + change, bid_total - refunded);
            }
        }
    }
}
