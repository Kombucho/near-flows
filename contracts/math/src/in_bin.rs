//! A swap inside ONE bin. Inside a bin the price is fixed (constant sum),
//! like a limit order: you get exactly `price` until the bin runs out.
//!
//! A bin holds `token` (what buyers take) and `quote` (what sellers take).
//! The fee is charged before this step, so `amount_in` here is net of fee.

use crate::wide::{mul_div_down, mul_div_up};
use crate::PRICE_SCALE;

/// Result of trading in one bin.
#[derive(Debug, PartialEq, Eq)]
pub struct Fill {
    pub used_in: u128, // how much of amount_in this bin consumed
    pub out: u128,     // how much the trader receives
}

/// Buy token with quote at `price` (quote per token, fixed-point).
pub fn buy_token(token_reserve: u128, price: u128, quote_in: u128) -> Fill {
    let want = mul_div_down(quote_in, PRICE_SCALE, price); // tokens quote_in can buy
    if want < token_reserve {
        return Fill { used_in: quote_in, out: want };
    }
    // Bin runs dry: take all its tokens and charge only what they cost (rounded up).
    let cost = mul_div_up(token_reserve, price, PRICE_SCALE);
    Fill { used_in: cost.min(quote_in), out: token_reserve }
}

/// Sell token for quote at `price`.
pub fn sell_token(quote_reserve: u128, price: u128, token_in: u128) -> Fill {
    let want = mul_div_down(token_in, price, PRICE_SCALE); // quote token_in is worth
    if want < quote_reserve {
        return Fill { used_in: token_in, out: want };
    }
    let cost = mul_div_up(quote_reserve, PRICE_SCALE, price);
    Fill { used_in: cost.min(token_in), out: quote_reserve }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P2: u128 = 2 * PRICE_SCALE; // 1 token costs 2 quote

    #[test]
    fn partial_fill_stays_in_bin() {
        // 100 quote at price 2 buys 50 tokens; the bin has 1000.
        assert_eq!(buy_token(1_000, P2, 100), Fill { used_in: 100, out: 50 });
    }

    #[test]
    fn exact_empty() {
        // 2000 quote buys exactly all 1000 tokens.
        assert_eq!(buy_token(1_000, P2, 2_000), Fill { used_in: 2_000, out: 1_000 });
    }

    #[test]
    fn overflowing_order_empties_bin_and_returns_rest() {
        // 5000 quote, but only 1000 tokens (cost 2000) are here. 3000 moves to the next bin.
        let f = buy_token(1_000, P2, 5_000);
        assert_eq!(f, Fill { used_in: 2_000, out: 1_000 });
    }

    #[test]
    fn never_pays_more_than_reserve() {
        for q in [0u128, 1, 3, 1_999, 2_000, 2_001, u64::MAX as u128] {
            let f = buy_token(1_000, P2, q);
            assert!(f.out <= 1_000 && f.used_in <= q);
        }
    }

    #[test]
    fn rounding_favours_pool() {
        // 3 quote at price 2 = 1.5 tokens -> trader gets 1.
        assert_eq!(buy_token(1_000, P2, 3).out, 1);
        // Selling 1 token at price 2 gives 2 quote; selling with a tiny bin caps.
        assert_eq!(sell_token(1, P2, 1), Fill { used_in: 1, out: 1 });
    }
}
