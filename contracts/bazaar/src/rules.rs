//! The rules, with no blockchain in them: what a buy gets and what it costs.
//! The contract (lib.rs) moves the real tokens.
//!
//! LISTINGS: a fixed price for a fixed amount. Buyers may take part of it at
//! the same price per unit; rounding always favours the seller by < 1 unit.
//!
//! FALLING-PRICE AUCTIONS (Dutch): the total price starts at `start` and falls
//! in a straight line to `floor` over the auction's duration, then stays at
//! the floor. Whoever buys first gets it at that moment's price (for tokens
//! and idea shares: each buyer takes what they pay for, while it lasts).
//! Why this format: nobody's purchase changes anyone else's price, so a
//! seller bidding against their own buyers from a second account (shill
//! bidding) can't push the price up. The only thing a fake buyer can do is
//! buy the item, and pay the fee on it.

use math::wide::{mul_div_down, mul_div_up};
use near_sdk::near;

pub const MIN_DURATION_NS: u64 = 60 * 60 * 1_000_000_000; // 1 hour
pub const MAX_DURATION_NS: u64 = 30 * 24 * 60 * 60 * 1_000_000_000; // 30 days

#[derive(Debug, PartialEq, Eq)]
pub enum RuleError {
    ZeroAmount,
    TooSmall,               // pays for less than one unit
    TooLow { min: u128 },   // pays less than the price of an indivisible item
    BadDuration,
    FloorAboveStart,
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

/// A falling price: the total ask goes from `start_ask` at `start_ns` to
/// `floor_ask` at `end_ns`, in a straight line, then stays at the floor.
#[near(serializers = [borsh])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Falling {
    pub start_ask: u128,
    pub floor_ask: u128,
    pub start_ns: u64,
    pub end_ns: u64,
}

impl Falling {
    pub fn new(start_ask: u128, floor_ask: u128, start_ns: u64, duration_ns: u64) -> Result<Self, RuleError> {
        if start_ask == 0 || floor_ask == 0 {
            return Err(RuleError::ZeroAmount);
        }
        if floor_ask > start_ask {
            return Err(RuleError::FloorAboveStart);
        }
        if !(MIN_DURATION_NS..=MAX_DURATION_NS).contains(&duration_ns) {
            return Err(RuleError::BadDuration);
        }
        Ok(Self { start_ask, floor_ask, start_ns, end_ns: start_ns + duration_ns })
    }

    /// The total ask at `now` (rounded up: the seller never gets less than the line).
    pub fn ask_at(&self, now: u64) -> u128 {
        if now >= self.end_ns {
            return self.floor_ask;
        }
        let elapsed = now.saturating_sub(self.start_ns) as u128;
        let span = (self.end_ns - self.start_ns) as u128;
        let drop = mul_div_down(self.start_ask - self.floor_ask, elapsed, span);
        self.start_ask - drop
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: u64 = 60 * 60 * 1_000_000_000;

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
    fn a_falling_price_drops_in_a_straight_line_then_holds_at_the_floor() {
        let f = Falling::new(1_000, 400, 10 * H, 6 * H).unwrap();
        assert_eq!(f.ask_at(0), 1_000, "before the start: the start price");
        assert_eq!(f.ask_at(10 * H), 1_000);
        assert_eq!(f.ask_at(13 * H), 700, "halfway: halfway down");
        assert_eq!(f.ask_at(16 * H), 400);
        assert_eq!(f.ask_at(1_000 * H), 400, "never below the floor");
        assert_eq!(Falling::new(400, 1_000, 0, 6 * H), Err(RuleError::FloorAboveStart));
        assert_eq!(Falling::new(1_000, 400, 0, H / 2), Err(RuleError::BadDuration));
        assert_eq!(Falling::new(1_000, 0, 0, 6 * H), Err(RuleError::ZeroAmount));
        assert_eq!(Falling::new(500, 500, 0, H).unwrap().ask_at(30 * H), 500, "start = floor: a plain listing");
    }

    #[test]
    fn the_price_never_rises_whatever_anyone_does() {
        // The ask depends on time only: no buy, bid or account can move it up.
        let f = Falling::new(10_000, 1, 0, 30 * 24 * H).unwrap();
        let mut last = u128::MAX;
        for t in (0..=31 * 24).map(|h| h as u64 * H) {
            let a = f.ask_at(t);
            assert!(a <= last && a >= 1);
            last = a;
        }
    }
}
