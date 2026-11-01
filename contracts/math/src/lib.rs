//! Pure integer math for the market. No near-sdk, no floats.
//!
//! Rounding rule everywhere: round against the user, in favour of the pool.
//! Outputs and claims round DOWN, required inputs and fees round UP.
//! The leftover "dust" stays in the contract, which is correct, not a bug.

pub mod acc;
pub mod cp;
pub mod fees;
pub mod in_bin;
pub mod price;
pub mod wide;

/// Fixed-point scale for prices: 1.0 is stored as 10^18.
pub const PRICE_SCALE: u128 = 1_000_000_000_000_000_000;

/// Basis points: 10_000 bps = 100%.
pub const BPS: u128 = 10_000;
