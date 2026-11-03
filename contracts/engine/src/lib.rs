//! The market engine. Plain Rust, no blockchain code.
//!
//! The NEAR contract will hold one `Book` in its state and call these methods.
//! Keeping the logic here means we can test and simulate it on a laptop.

mod audit;
mod book;
mod crank;
mod creator;
mod error;
mod ladder;
mod launch;
mod position;
mod swap;
mod tp;
mod vault;

pub use book::{Bin, Book, Config, CreatorPool, Pots, Position, PROTOCOL};
pub use crank::CrankReport;
pub use error::Error;
pub use ladder::{Buy, Rung, DEFAULT_LADDER, LAUNCH_LADDER};
pub use position::{Settlement, Withdrawal};
pub use swap::{Side, SwapResult};
pub use vault::{Staker, Vault, Vote, DEFAULT_VOTE};
