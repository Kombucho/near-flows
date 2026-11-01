//! The market engine. Plain Rust, no blockchain code.
//!
//! The NEAR contract will hold one `Book` in its state and call these methods.
//! Keeping the logic here means we can test and simulate it on a laptop.

mod book;
mod error;
mod position;
mod swap;

pub use book::{Bin, Book, Config, Pots, Position};
pub use error::Error;
pub use position::Withdrawal;
pub use swap::{Side, SwapResult};
