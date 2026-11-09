//! Test-only stand-ins, so vault tests are deterministic:
//! - swap: send a token with ft_transfer_call, msg {"token_out", "min_out"}; you get
//!   amount * rate back from the reserves, or the whole amount refunded if that's
//!   below min_out (like a Rhea swap that fails its slippage check).
//!   With `"use_bps"` in the msg it swaps only that share of the amount and
//!   refunds the rest (a partial fill).
//! - stake: `deposit_and_stake` (payable) hands out `lst` tokens 1:1 from reserves.
//! - `get_return` view, the shape a price trigger reads.
//! Anyone can set rates: it's a mock.

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, PromiseOrValue};

const SCALE: u128 = 1_000_000_000_000_000_000_000_000;

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct MockDex {
    rates: LookupMap<String, u128>,
    lst: AccountId,
    /// Refuse every swap (to test failures).
    broken: bool,
}

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde")]
struct SwapMsg {
    token_out: AccountId,
    min_out: U128,
    /// Use only this share of the amount (bps); the rest is refunded.
    #[serde(default)]
    use_bps: Option<u16>,
}

#[near]
impl MockDex {
    #[init]
    pub fn new(lst: AccountId) -> Self {
        Self { rates: LookupMap::new(b"r"), lst, broken: false }
    }

    /// Output per input, scaled 1e24.
    pub fn set_rate(&mut self, token_in: AccountId, token_out: AccountId, rate: U128) {
        self.rates.insert(format!("{token_in}>{token_out}"), rate.0);
    }

    pub fn set_broken(&mut self, broken: bool) {
        self.broken = broken;
    }

    pub fn get_return(&self, token_in: AccountId, amount_in: U128, token_out: AccountId) -> U128 {
        U128(math::wide::mul_div_down(amount_in.0, self.rates.get(&format!("{token_in}>{token_out}")).copied().unwrap_or(0), SCALE))
    }

    #[payable]
    pub fn deposit_and_stake(&mut self) {
        require!(!self.broken, "pool closed");
        let amount = env::attached_deposit().as_yoctonear();
        ext_ft_core::ext(self.lst.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(10))
            .ft_transfer(env::predecessor_account_id(), U128(amount), None)
            .detach();
    }
}

#[near]
impl FungibleTokenReceiver for MockDex {
    fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let m: SwapMsg = near_sdk::serde_json::from_str(&msg).unwrap_or_else(|_| env::panic_str("bad msg"));
        let used = m.use_bps.map_or(amount.0, |b| amount.0 * b as u128 / 10_000);
        let out = self.get_return(env::predecessor_account_id(), U128(used), m.token_out.clone()).0;
        require!(!self.broken, "swap broken");
        require!(out > 0 && out >= m.min_out.0, "slippage");
        ext_ft_core::ext(m.token_out)
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(Gas::from_tgas(10))
            .ft_transfer(sender_id, U128(out), None)
            .detach();
        PromiseOrValue::Value(U128(amount.0 - used))
    }
}
