//! Two ways a coin enters the market.

use math::wide::mul_div_down;
use math::PRICE_SCALE;

use crate::{Book, Config, Error};

impl Book {
    /// meme.cooking-style graduation: an auction raised `raised_quote` and
    /// `lp_tokens` go to the pool. The opening price is the auction price
    /// (raised / lp_tokens), and all of it becomes protocol liquidity spread over
    /// `n` bins per side, locked forever.
    pub fn graduate(mut cfg: Config, raised_quote: u128, lp_tokens: u128, n: i32, now_ns: u64) -> Book {
        assert!(raised_quote > 0 && lp_tokens > 0, "empty auction");
        cfg.base_price = mul_div_down(raised_quote, PRICE_SCALE, lp_tokens);
        let mut b = Book::new(cfg);
        b.launched_ns = now_ns;
        b.add_protocol_uniform(lp_tokens, raised_quote, n);
        b
    }

    /// For a coin that already trades elsewhere: someone (a dev team, a
    /// community) seeds liquidity at `cfg.base_price`. Locking it for at least
    /// `cfg.min_creator_lock_ns` makes it earn the creator slice.
    pub fn seed(
        cfg: Config, owner: &str, token: u128, quote: u128, n: i32, locked_until_ns: u64, now_ns: u64,
    ) -> Result<(Book, Vec<u64>), Error> {
        let mut b = Book::new(cfg);
        b.launched_ns = now_ns;
        let ids = b.add_uniform(owner, token, quote, n, locked_until_ns, now_ns)?;
        Ok((b, ids))
    }
}
