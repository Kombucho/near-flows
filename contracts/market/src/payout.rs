//! Sending tokens out, safely.
//!
//! Order matters (reentrancy rule): the engine's state is already updated
//! BEFORE we send. If the transfer then fails (most often: the receiver is
//! not registered on the token contract), the callback records the amount as
//! "owed" so the user can pull it later with `withdraw_owed`. Nothing is lost
//! and nothing can be paid twice.
//!
//! On NEAR a cross-contract call is a Promise: it runs in a later block, and
//! `.then(...)` schedules our callback to run after it with its result.

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, AccountId, Gas, NearToken, PromiseError};

use crate::{Market, MarketExt};

const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_CALLBACK: Gas = Gas::from_tgas(5);

fn owed_key(token: &AccountId, who: &AccountId) -> String {
    format!("{token}|{who}")
}

#[near]
impl Market {
    /// Callback after a payout. `#[private]`: only this contract may call it.
    /// It never panics: a panic here could lose track of the money.
    #[private]
    pub fn on_payout(&mut self, token: AccountId, to: AccountId, amount: U128, #[callback_result] result: Result<(), PromiseError>) {
        if result.is_err() {
            let key = owed_key(&token, &to);
            let prev = self.owed.get(&key).copied().unwrap_or(0);
            self.owed.insert(key, prev + amount.0);
            self.emit("payout_failed", &format!(r#"{{"token":"{token}","to":"{to}","amount":"{}"}}"#, amount.0));
        }
    }

    /// Retry a payout that failed earlier (register on the token first).
    #[payable]
    pub fn withdraw_owed(&mut self, token: AccountId) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let key = owed_key(&token, &who);
        let amount = self.owed.get(&key).copied().unwrap_or(0);
        near_sdk::require!(amount > 0, "nothing owed");
        self.owed.insert(key, 0); // deduct first, the callback restores on failure
        self.pay(&token, &who, amount);
    }

    pub fn owed_of(&self, token: AccountId, account_id: AccountId) -> U128 {
        U128(self.owed.get(&owed_key(&token, &account_id)).copied().unwrap_or(0))
    }
}

impl Market {
    /// Send `amount` of `token` to `to`, with the failure callback attached.
    pub(crate) fn pay(&self, token: &AccountId, to: &AccountId, amount: u128) {
        if amount == 0 {
            return;
        }
        ext_ft_core::ext(token.clone())
            .with_attached_deposit(NearToken::from_yoctonear(1)) // NEP-141 requires exactly 1 yocto
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(to.clone(), U128(amount), None)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_payout(token.clone(), to.clone(), U128(amount)),
            )
            .detach(); // runs on its own; the callback handles failure
    }

    /// Pay out a (quote, token) pair.
    pub(crate) fn pay_both(&self, to: &AccountId, quote: u128, token: u128) {
        let (q, t) = (self.quote.clone(), self.token.clone());
        self.pay(&q, to, quote);
        self.pay(&t, to, token);
    }
}
