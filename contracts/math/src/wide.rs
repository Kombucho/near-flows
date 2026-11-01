//! `a * b / d` without overflowing.
//!
//! Two u128 numbers multiplied can need up to 256 bits, so we do the
//! multiplication in a 256-bit integer (U256) and only come back to u128
//! after dividing.

use uint::construct_uint;

construct_uint! {
    /// 256-bit unsigned integer (4 x 64-bit words).
    pub struct U256(4);
}

/// floor(a * b / d). Panics if d == 0 or the result does not fit in u128.
pub fn mul_div_down(a: u128, b: u128, d: u128) -> u128 {
    assert!(d != 0, "division by zero");
    let r = U256::from(a) * U256::from(b) / U256::from(d);
    to_u128(r)
}

/// ceil(a * b / d). Same as above but rounds up when there is a remainder.
pub fn mul_div_up(a: u128, b: u128, d: u128) -> u128 {
    assert!(d != 0, "division by zero");
    let num = U256::from(a) * U256::from(b);
    let den = U256::from(d);
    let (q, rem) = num.div_mod(den);
    let q = if rem.is_zero() { q } else { q + U256::one() };
    to_u128(q)
}

fn to_u128(x: U256) -> u128 {
    assert!(x <= U256::from(u128::MAX), "overflow: result exceeds u128");
    x.as_u128()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_down_and_up() {
        assert_eq!(mul_div_down(10, 10, 3), 33); // 100/3 = 33.33
        assert_eq!(mul_div_up(10, 10, 3), 34);
        assert_eq!(mul_div_up(10, 3, 3), 10); // exact: no rounding
    }

    #[test]
    fn survives_huge_intermediate() {
        // u128::MAX * 2 overflows u128, but /2 brings it back.
        assert_eq!(mul_div_down(u128::MAX, 2, 2), u128::MAX);
    }

    #[test]
    #[should_panic(expected = "overflow")]
    fn panics_when_result_too_big() {
        mul_div_down(u128::MAX, 2, 1);
    }
}
