//! A fake brain for relay tests: `set_answer` stores raw bytes (the call's
//! input), `handle` returns them to whichever relay asks. Anyone can set it.

use near_sdk::{env, near, PanicOnDefault};

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockBrain {}

#[near]
impl MockBrain {
    #[init]
    pub fn new() -> Self {
        Self {}
    }

    pub fn set_answer(&self) {
        env::storage_write(b"a", &env::input().unwrap_or_default());
    }

    pub fn handle(&self) {
        env::value_return(&env::storage_read(b"a").unwrap_or_default());
    }
}
