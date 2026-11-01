//! Fee-per-share accounting: pay thousands of holders without looping over them.
//!
//! Think of a counter "fees earned by ONE share since the beginning".
//! When a fee arrives we bump the counter. Each holder remembers the counter
//! value when they last settled (their snapshot). What they are owed is:
//!     shares * (counter_now - snapshot)
//! So paying everyone costs one addition, no matter how many holders exist.

use crate::wide::mul_div_down;

/// Extra precision for the counter, so tiny fees over many shares are not lost.
pub const ACC_SCALE: u128 = 1_000_000_000_000_000_000_000_000; // 1e24

/// Bump the counter by `fee` spread over `total_shares`. Rounds DOWN:
/// the undistributed remainder stays in the contract as dust.
pub fn accrue(acc: u128, fee: u128, total_shares: u128) -> u128 {
    if total_shares == 0 {
        return acc; // nobody to pay; caller decides where such fees go
    }
    acc.checked_add(mul_div_down(fee, ACC_SCALE, total_shares))
        .expect("accumulator overflow")
}

/// What a holder with `shares` is owed since `snapshot`. Rounds DOWN.
pub fn owed(shares: u128, acc_now: u128, snapshot: u128) -> u128 {
    let delta = acc_now.checked_sub(snapshot).expect("snapshot ahead of accumulator");
    mul_div_down(shares, delta, ACC_SCALE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worked_example_300_100_41() {
        // Alice 300 shares, Bob 100. A fee of 41 arrives.
        let acc = accrue(0, 41, 400);
        let alice = owed(300, acc, 0); // 30.75 -> 30
        let bob = owed(100, acc, 0); // 10.25 -> 10
        assert_eq!((alice, bob), (30, 10));
        assert!(alice + bob <= 41); // 1 unit of dust stays in the contract
    }

    #[test]
    fn late_joiner_gets_nothing_from_before() {
        let acc1 = accrue(0, 1_000, 100); // only the first holder is in
        let carol_snapshot = acc1; // Carol joins now
        let acc2 = accrue(acc1, 500, 200); // 100 old + 100 Carol
        assert_eq!(owed(100, acc2, carol_snapshot), 250);
        assert_eq!(owed(100, acc2, 0), 1_000 + 250);
    }

    #[test]
    fn claims_never_exceed_fees() {
        let mut acc = 0;
        let mut total_fees = 0u128;
        for fee in [1u128, 7, 13, 999, 12_345] {
            acc = accrue(acc, fee, 3); // 3 holders of 1 share each, worst case for rounding
            total_fees += fee;
        }
        assert!(3 * owed(1, acc, 0) <= total_fees);
    }

    #[test]
    fn no_holders_no_change() {
        assert_eq!(accrue(42, 1_000, 0), 42);
    }
}
