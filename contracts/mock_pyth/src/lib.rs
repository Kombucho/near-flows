//! Test-only Pyth: the real receiver's read interface and Price format
//! (pyth-crosschain target_chains/near/receiver), with prices set by hand
//! instead of pushed as Wormhole-signed updates. It also stands in for a
//! feed's second publisher (a vault's backup): see `set_price`.

use near_sdk::json_types::{I64, U64};
use near_sdk::store::LookupMap;
use near_sdk::{env, near, PanicOnDefault};

#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct Price {
    pub price: I64,
    pub conf: U64,
    pub expo: i32,
    pub publish_time: i64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockPyth {
    spot: LookupMap<String, Price>,
}

#[near]
impl MockPyth {
    #[init]
    pub fn new() -> Self {
        Self { spot: LookupMap::new(b"s") }
    }

    /// Set a feed's price, published `age_s` seconds ago. `backup`: what a second,
    /// independent publisher says for the same asset at the same time, served
    /// under `backup_id(price_id)` (the way a price board serves a Pyth id and a
    /// Chainlink id side by side).
    pub fn set_price(&mut self, price_id: String, price: I64, conf: U64, expo: i32, age_s: i64, backup: Option<I64>) {
        let t = (env::block_timestamp() / 1_000_000_000) as i64 - age_s;
        if let Some(b) = backup {
            self.spot.insert(reversed(&price_id), Price { price: b, conf, expo, publish_time: t });
        }
        self.spot.insert(price_id, Price { price, conf, expo, publish_time: t });
    }

    /// Where `set_price` puts a feed's backup: its id, reversed.
    pub fn backup_id(&self, price_id: String) -> String {
        reversed(&price_id)
    }

    pub fn get_price_no_older_than(&self, price_id: String, age: u64) -> Option<Price> {
        fresh(self.spot.get(&price_id), age)
    }
}

fn reversed(id: &str) -> String {
    id.chars().rev().collect()
}

/// Like the real receiver: no price if it's older than `age` seconds.
fn fresh(p: Option<&Price>, age: u64) -> Option<Price> {
    let now = env::block_timestamp() / 1_000_000_000;
    p.filter(|p| now.abs_diff(p.publish_time as u64) <= age).cloned()
}
