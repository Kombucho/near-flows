//! Bin prices. Bin `id` has price  base * (1 + step)^id.
//!
//! Example with step = 100 bps (1%): bin 0 = base, bin 1 = base * 1.01,
//! bin -1 = base / 1.01, bin 70 is roughly 2x base.

use crate::wide::mul_div_down;
use crate::{BPS, PRICE_SCALE};

/// (1 + step)^n as a fixed-point number (1.0 = PRICE_SCALE), for n >= 0.
/// Exponentiation by squaring: about 2*log2(n) multiplications instead of n.
fn growth(step_bps: u16, mut n: u32) -> u128 {
    let mut base = PRICE_SCALE + PRICE_SCALE * step_bps as u128 / BPS; // 1 + step
    let mut acc = PRICE_SCALE; // 1.0
    while n > 0 {
        if n & 1 == 1 {
            acc = mul_div_down(acc, base, PRICE_SCALE);
        }
        base = mul_div_down(base, base, PRICE_SCALE);
        n >>= 1;
    }
    acc
}

/// Price ratio of bin `id` relative to bin 0, fixed-point. Works for negative ids.
pub fn ratio(step_bps: u16, id: i32) -> u128 {
    let g = growth(step_bps, id.unsigned_abs());
    if id >= 0 {
        g
    } else {
        // 1 / g, still fixed-point: SCALE * SCALE / g
        mul_div_down(PRICE_SCALE, PRICE_SCALE, g)
    }
}

/// Absolute price of bin `id` (quote units per token unit, fixed-point).
pub fn bin_price(base_price: u128, step_bps: u16, id: i32) -> u128 {
    mul_div_down(base_price, ratio(step_bps, id), PRICE_SCALE)
}

/// How many bins above the entry bin sit at `multiple_bps` of the entry price.
/// Smallest k with (1+step)^k >= multiple. Used for take-profit ladders
/// (multiple_bps = 20_000 means 2x). Binary search, so no floats or logs.
pub fn bins_for_multiple(step_bps: u16, multiple_bps: u32) -> i32 {
    let target = PRICE_SCALE * multiple_bps as u128 / BPS;
    // Find an upper bound by doubling, so we never compute a price far past the
    // target (a fixed huge bound like bin 100_000 would overflow u128).
    let mut hi = 1i32;
    while ratio(step_bps, hi) < target {
        hi *= 2;
    }
    let mut lo = 0i32;
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if ratio(step_bps, mid) >= target {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    fn as_f64(x: u128) -> f64 {
        x as f64 / PRICE_SCALE as f64
    }

    #[test]
    fn bin_zero_is_base() {
        assert_eq!(ratio(100, 0), PRICE_SCALE);
        assert_eq!(bin_price(5 * PRICE_SCALE, 100, 0), 5 * PRICE_SCALE);
    }

    #[test]
    fn matches_float_within_tolerance() {
        // Floats are only allowed in tests, as an independent check.
        for id in [-500, -70, -1, 1, 70, 162, 500] {
            let exact = 1.01f64.powi(id);
            let ours = as_f64(ratio(100, id));
            assert!((ours / exact - 1.0).abs() < 1e-12, "bin {id}: {ours} vs {exact}");
        }
    }

    #[test]
    fn up_then_down_is_roughly_one() {
        let up = ratio(100, 70);
        let down = ratio(100, -70);
        let product = as_f64(up) * as_f64(down);
        assert!((product - 1.0).abs() < 1e-12);
    }

    #[test]
    fn ladder_offsets_at_1pct_step() {
        // ln(m)/ln(1.01), rounded up: 1.5x->40.75, 2x->69.66, 3x->110.41, 5x->161.75
        assert_eq!(bins_for_multiple(100, 15_000), 41);
        assert_eq!(bins_for_multiple(100, 20_000), 70);
        assert_eq!(bins_for_multiple(100, 30_000), 111);
        assert_eq!(bins_for_multiple(100, 50_000), 162);
    }
}
