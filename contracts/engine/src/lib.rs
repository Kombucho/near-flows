//! The market engine. Plain Rust, no blockchain code.
//!
//! The NEAR contract holds one `Book` in its state and calls these methods.
//! With feature "near" the Book's big collections become lazy on-chain maps (store.rs).
//! Keeping the logic here means we can test and simulate it on a laptop.

mod audit;
mod bid;
mod book;
mod crank;
mod creator;
mod error;
mod ladder;
mod launch;
mod position;
mod range;
mod store;
mod swap;
mod tp;
mod vault;

pub use bid::{DEFAULT_DIPS, MAX_DIPS, MIN_DIP, WHEEL_EXITS};
pub use book::{Bin, Book, Config, CreatorPool, Pots, Position, PROTOCOL};
pub use crank::CrankReport;
pub use error::Error;
pub use ladder::{Buy, Rung, DEFAULT_LADDER, LAUNCH_LADDER};
pub use position::{Settlement, Withdrawal};
pub use range::{spread, LpBuy, MAX_RANGE_BINS};
pub use store::Map;
pub use swap::{Side, SwapResult};
pub use vault::{Staker, Vault, Vote, DEFAULT_VOTE};
