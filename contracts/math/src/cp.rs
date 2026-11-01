//! Constant-product pool (x * y = k), the kind meme.cooking graduates into on Ref.
//! We use it to compare routes: "would this trade be cheaper on Ref or in our bins?"

use crate::wide::{mul_div_down, mul_div_up};
use crate::BPS;

/// Output for putting `amount_in` into a pool with reserves (reserve_in, reserve_out),
/// after a fee of `fee_bps`. Rounded DOWN.
pub fn cp_out(reserve_in: u128, reserve_out: u128, amount_in: u128, fee_bps: u16) -> u128 {
    let net_in = amount_in - mul_div_up(amount_in, fee_bps as u128, BPS);
    // out = reserve_out * net_in / (reserve_in + net_in)
    mul_div_down(reserve_out, net_in, reserve_in + net_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textbook_example() {
        // x = y = 1000, put in 100, no fee: 1000 * 100 / 1100 = 90.9 -> 90
        assert_eq!(cp_out(1_000, 1_000, 100, 0), 90);
    }

    #[test]
    fn fee_reduces_output() {
        // 0.1% fee (meme.cooking's Ref pools): net_in = 100 - 1 (rounded up) = 99
        assert_eq!(cp_out(1_000, 1_000, 100, 10), 90); // 1000*99/1099 = 90.08
        assert!(cp_out(1_000_000, 1_000_000, 100_000, 100) < cp_out(1_000_000, 1_000_000, 100_000, 0));
    }

    #[test]
    fn k_never_decreases() {
        let (x, y) = (5_000u128, 7_000u128);
        for dx in [1u128, 10, 333, 4_999, 50_000] {
            let dy = cp_out(x, y, dx, 10);
            assert!((x + dx) * (y - dy) >= x * y);
        }
    }
}
