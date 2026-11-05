//! Sending NEAR, tokens and NFTs out, safely.
//!
//! State is always updated BEFORE anything is sent. If a token or NFT
//! transfer fails (e.g. the receiver is not registered on the token), the
//! callback records it as owed, and the receiver pulls it later.

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_contract_standards::non_fungible_token::core::ext_nft_core;
use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, require, AccountId, Gas, NearToken, Promise, PromiseError};

use crate::{Bazaar, BazaarExt, Item};

const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_NFT_TRANSFER: Gas = Gas::from_tgas(20);
const GAS_CALLBACK: Gas = Gas::from_tgas(5);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);

fn owed_key(asset: &Option<AccountId>, who: &AccountId) -> String {
    match asset {
        Some(t) => format!("{t}|{who}"),
        None => format!("near|{who}"),
    }
}

#[near]
impl Bazaar {
    /// Callback after a token payout. Never panics.
    #[private]
    pub fn on_payout(&mut self, token: AccountId, to: AccountId, amount: U128, #[callback_result] result: Result<(), PromiseError>) {
        if result.is_err() {
            self.add_owed(&Some(token.clone()), &to, amount.0);
            self.emit("payout_failed", &format!(r#"{{"token":"{token}","to":"{to}","amount":"{}"}}"#, amount.0));
        }
    }

    /// Callback after an NFT delivery. Never panics.
    #[private]
    pub fn on_nft_payout(&mut self, nft_contract: AccountId, token_id: String, to: AccountId, #[callback_result] result: Result<(), PromiseError>) {
        if result.is_err() {
            self.owed_nft.insert(format!("{nft_contract}|{token_id}"), to.clone());
            self.emit("nft_payout_failed", &format!(r#"{{"contract":"{nft_contract}","token_id":"{token_id}","to":"{to}"}}"#));
        }
    }

    /// Retry a token payout that failed earlier (register on the token first).
    #[payable]
    pub fn withdraw_owed(&mut self, token: AccountId) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let asset = Some(token);
        let key = owed_key(&asset, &who);
        let amount = self.owed.get(&key).copied().unwrap_or(0);
        require!(amount > 0, "nothing owed");
        self.owed.remove(&key); // deduct first; the callback restores it on failure
        self.pay(&asset, &who, amount);
    }

    /// Retry an NFT delivery that failed earlier.
    #[payable]
    pub fn withdraw_owed_nft(&mut self, nft_contract: AccountId, token_id: String) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let key = format!("{nft_contract}|{token_id}");
        require!(self.owed_nft.get(&key) == Some(&who), "not owed to you");
        self.owed_nft.remove(&key);
        self.give(&Item::Nft { contract: nft_contract, token_id }, &who);
    }

    pub fn owed_of(&self, token: AccountId, account_id: AccountId) -> U128 {
        U128(self.owed.get(&owed_key(&Some(token), &account_id)).copied().unwrap_or(0))
    }
}

impl Bazaar {
    pub(crate) fn add_owed(&mut self, asset: &Option<AccountId>, who: &AccountId, amount: u128) {
        let key = owed_key(asset, who);
        let prev = self.owed.get(&key).copied().unwrap_or(0);
        self.owed.insert(key, prev + amount);
    }

    /// Send `amount` of `asset` (None = native NEAR) to `to`.
    pub(crate) fn pay(&self, asset: &Option<AccountId>, to: &AccountId, amount: u128) {
        if amount == 0 {
            return;
        }
        match asset {
            None => Promise::new(to.clone()).transfer(NearToken::from_yoctonear(amount)).detach(),
            Some(token) => ext_ft_core::ext(token.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_FT_TRANSFER)
                .ft_transfer(to.clone(), U128(amount), None)
                .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_payout(token.clone(), to.clone(), U128(amount)))
                .detach(),
        }
    }

    /// Hand an item to `to`. An idea has nothing to hand over.
    pub(crate) fn give(&self, item: &Item, to: &AccountId) {
        match item {
            Item::Ft { token, amount } => self.pay(&Some(token.clone()), to, amount.0),
            Item::Nft { contract, token_id } => ext_nft_core::ext(contract.clone())
                .with_attached_deposit(ONE_YOCTO)
                .with_static_gas(GAS_NFT_TRANSFER)
                .nft_transfer(to.clone(), token_id.clone(), None, None)
                .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK)
                    .on_nft_payout(contract.clone(), token_id.clone(), to.clone()))
                .detach(),
            Item::Idea => {}
        }
    }
}
