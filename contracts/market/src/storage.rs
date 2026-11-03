//! Storage: on NEAR, every byte a contract stores locks some NEAR. Users
//! prepay a storage balance; each action that adds bytes (a position, a stake)
//! is charged from it, and actions that free bytes credit it back.
//! NEP-145 style: storage_deposit / storage_balance_of / storage_withdraw.

use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, require, AccountId, NearToken, Promise};

use crate::{Market, MarketExt};

/// Suggested minimum deposit: enough for a handful of positions.
const MIN_DEPOSIT: u128 = 50_000_000_000_000_000_000_000; // 0.05 NEAR

#[near]
impl Market {
    /// Add to an account's storage balance (defaults to the caller).
    #[payable]
    pub fn storage_deposit(&mut self, account_id: Option<AccountId>) -> U128 {
        let who = account_id.unwrap_or_else(env::predecessor_account_id);
        let amount = env::attached_deposit().as_yoctonear();
        require!(amount > 0, "attach NEAR to deposit");
        let bal = self.storage.get(&who).copied().unwrap_or(0) + amount;
        self.storage.insert(who, bal);
        U128(bal)
    }

    pub fn storage_balance_of(&self, account_id: AccountId) -> U128 {
        U128(self.storage.get(&account_id).copied().unwrap_or(0))
    }

    pub fn storage_balance_min(&self) -> U128 {
        U128(MIN_DEPOSIT)
    }

    /// Take back unused storage balance. 1 yoctoNEAR proves a full-access key signed.
    #[payable]
    pub fn storage_withdraw(&mut self, amount: Option<U128>) -> U128 {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let bal = self.storage.get(&who).copied().unwrap_or(0);
        let take = amount.map(|a| a.0).unwrap_or(bal);
        require!(take <= bal, "more than your storage balance");
        self.storage.insert(who.clone(), bal - take);
        Promise::new(who).transfer(NearToken::from_yoctonear(take)).detach();
        U128(bal - take)
    }
}

impl Market {
    /// Charge `who` for bytes added since `before` (or credit bytes freed).
    /// Call it at the end of every user action that can change storage.
    /// If the balance is short this panics, which on NEAR reverts the whole
    /// call (and an ft_on_transfer revert refunds the tokens).
    /// Like charge_storage, but returns false instead of panicking when the
    /// balance is short (delegated buys must never panic).
    pub(crate) fn try_charge_storage(&mut self, who: &AccountId, before: u64) -> bool {
        env::state_write(self);
        let after = env::storage_usage();
        let cost = after.saturating_sub(before) as u128 * env::storage_byte_cost().as_yoctonear();
        let bal = self.storage.get(who).copied().unwrap_or(0);
        if bal < cost {
            return false;
        }
        self.storage.insert(who.clone(), bal - cost);
        true
    }

    pub(crate) fn charge_storage(&mut self, who: &AccountId, before: u64) {
        // NEAR saves the contract state only when the method returns, so
        // storage_usage() would not include this call's changes yet. Save the
        // state now so the measurement is real. (Found by the unit tests:
        // without this every action looked free and nobody paid for storage.)
        env::state_write(self);
        let after = env::storage_usage();
        let byte_cost = env::storage_byte_cost().as_yoctonear();
        let bal = self.storage.get(who).copied().unwrap_or(0);
        let bal = if after > before {
            let cost = (after - before) as u128 * byte_cost;
            require!(bal >= cost, "not enough storage balance: call storage_deposit first");
            bal - cost
        } else {
            bal + (before - after) as u128 * byte_cost
        };
        self.storage.insert(who.clone(), bal);
    }
}
