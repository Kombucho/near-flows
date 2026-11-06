//! Test-only Pyth: the real receiver's read interface and Price format
//! (pyth-crosschain target_chains/near/receiver), with prices set by hand
//! instead of pushed as Wormhole-signed updates.

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
    ema: LookupMap<String, Price>,
}

#[near]
impl MockPyth {
    #[init]
    pub fn new() -> Self {
        Self { spot: LookupMap::new(b"s"), ema: LookupMap::new(b"e") }
    }

    /// Set a feed's price and EMA, published `age_s` seconds ago.
    pub fn set_price(&mut self, price_id: String, price: I64, conf: U64, expo: i32, ema: I64, age_s: i64) {
        let t = (env::block_timestamp() / 1_000_000_000) as i64 - age_s;
        self.spot.insert(price_id.clone(), Price { price, conf, expo, publish_time: t });
        self.ema.insert(price_id, Price { price: ema, conf, expo, publish_time: t });
    }

    pub fn get_price_no_older_than(&self, price_id: String, age: u64) -> Option<Price> {
        fresh(self.spot.get(&price_id), age)
    }

    pub fn get_ema_price_no_older_than(&self, price_id: String, age: u64) -> Option<Price> {
        fresh(self.ema.get(&price_id), age)
    }
}

/// Like the real receiver: no price if it's older than `age` seconds.
fn fresh(p: Option<&Price>, age: u64) -> Option<Price> {
    let now = env::block_timestamp() / 1_000_000_000;
    p.filter(|p| now.abs_diff(p.publish_time as u64) <= age).cloned()
}
