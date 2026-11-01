//! Everything that can go wrong, as values instead of crashes.
//! `Result<T, Error>` is Rust's version of "returns T or throws Error",
//! except the caller is forced by the compiler to handle the error case.

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    ZeroAmount,
    /// Liquidity on the wrong side of the price: tokens must sit at or above
    /// the active bin (asks), quote at or below it (bids).
    WrongSide,
    /// The trade would return less than the trader's minimum.
    Slippage { out: u128, min_out: u128 },
    NoLiquidity,
    NoPosition,
    NotOwner,
    Locked { until_ns: u64 },
    /// Crank called before its interval passed.
    TooSoon { next_ns: u64 },
    NotStaker,
    NotEnoughStake,
    BadVote,
    /// Staking, voting and the creator slice exist only for native coins.
    NotNative,
}
