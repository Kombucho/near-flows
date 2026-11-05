//! Receiving tokens and NFTs, with a `msg` saying what to do with them.
//!
//! Tokens (`ft_transfer_call` on the token, receiver = this contract):
//!   {"action":"sell","sale":"listing","ask":"5000","pay":"wrap.testnet","lore":{"title":"..."}}
//!   {"action":"sell","sale":"auction","opening":"100","reserve":"1000","hours":48,"lore":{...}}
//!        list or auction the tokens sent. "pay" omitted = paid in native NEAR.
//!   {"action":"buy","lot":7}      pay for a listing priced in this token (excess refunded)
//!   {"action":"bid","lot":7}      bid on an auction priced in this token (amount = your maximum)
//! NFTs (`nft_transfer_call` on the NFT contract): only "sell", same fields.
//!
//! If anything fails the call aborts and the token contract refunds the
//! sender in full (for NFTs, the NFT contract gives the NFT back).

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::non_fungible_token::core::NonFungibleTokenReceiver;
use near_contract_standards::non_fungible_token::TokenId;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::{env, near, require, AccountId, PromiseOrValue};

use crate::{Bazaar, BazaarExt, Item, Lore, SaleTerms};

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", tag = "action", rename_all = "snake_case")]
enum Msg {
    Sell {
        #[serde(flatten)]
        terms: SaleTerms,
        #[serde(default)]
        pay: Option<AccountId>,
        lore: Lore,
    },
    Buy { lot: u64 },
    Bid { lot: u64 },
}

fn parse(msg: &str) -> Msg {
    near_sdk::serde_json::from_str(msg).unwrap_or_else(|e| env::panic_str(&format!("bad msg: {e}")))
}

#[near]
impl FungibleTokenReceiver for Bazaar {
    fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        let refund = match parse(&msg) {
            Msg::Sell { terms, pay, lore } => {
                let before = env::storage_usage();
                self.create(sender_id.clone(), Item::Ft { token, amount }, terms, pay, lore);
                self.charge_storage(&sender_id, before);
                0
            }
            Msg::Buy { lot } => {
                require!(self.lot(lot).pay.as_ref() == Some(&token), "this lot is not paid in this token");
                self.on_buy(lot, &sender_id, amount.0)
            }
            Msg::Bid { lot } => {
                require!(self.lot(lot).pay.as_ref() == Some(&token), "this lot is not paid in this token");
                self.on_bid(lot, &sender_id, amount.0);
                0
            }
        };
        PromiseOrValue::Value(U128(refund))
    }
}

#[near]
impl NonFungibleTokenReceiver for Bazaar {
    /// Returns true to send the NFT back, false to keep it (it is now for sale).
    fn nft_on_transfer(
        &mut self, sender_id: AccountId, previous_owner_id: AccountId, token_id: TokenId, msg: String,
    ) -> PromiseOrValue<bool> {
        let _ = sender_id; // may be an approved operator; the seller is the owner
        let contract = env::predecessor_account_id();
        let Msg::Sell { terms, pay, lore } = parse(&msg) else { env::panic_str("NFTs can only be sold") };
        let before = env::storage_usage();
        self.create(previous_owner_id.clone(), Item::Nft { contract, token_id }, terms, pay, lore);
        self.charge_storage(&previous_owner_id, before);
        PromiseOrValue::Value(false)
    }
}
