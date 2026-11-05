//! Receiving tokens and NFTs, with a `msg` saying what to do with them.
//!
//! Selling tokens or an NFT takes two steps, so nobody can open a sale in
//! someone else's name: `open_sale` (signed by the seller), then the item:
//!   {"action":"fund","lot":7}     via ft_transfer_call or nft_transfer_call
//! Tokens (`ft_transfer_call` on the token, receiver = this contract):
//!   {"action":"buy","lot":7}      pay for a listing priced in this token (excess refunded)
//!   {"action":"bid","lot":7}      bid on an auction priced in this token (amount = your maximum)
//!   {"action":"deliver","lot":7}  seller of an idea: deliver the pledged tokens (ideas.rs)
//!
//! If anything fails the call aborts and the token contract refunds the
//! sender in full (for NFTs, the NFT contract gives the NFT back).

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::non_fungible_token::core::NonFungibleTokenReceiver;
use near_contract_standards::non_fungible_token::TokenId;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::{env, near, require, AccountId, PromiseOrValue};

use crate::{Bazaar, BazaarExt};

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", tag = "action", rename_all = "snake_case")]
enum Msg {
    Fund { lot: u64 },
    Buy { lot: u64 },
    Bid { lot: u64 },
    Deliver { lot: u64 },
}

fn parse(msg: &str) -> Msg {
    near_sdk::serde_json::from_str(msg).unwrap_or_else(|e| env::panic_str(&format!("bad msg: {e}")))
}

#[near]
impl FungibleTokenReceiver for Bazaar {
    fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let token = env::predecessor_account_id();
        let refund = match parse(&msg) {
            Msg::Fund { lot } => {
                self.fund(lot, &sender_id, &token, amount.0, None);
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
            Msg::Deliver { lot } => self.deliver(lot, &sender_id, token, amount.0),
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
        let Msg::Fund { lot } = parse(&msg) else { env::panic_str("send an NFT only to fund a sale") };
        self.fund(lot, &previous_owner_id, &contract, 1, Some(&token_id));
        PromiseOrValue::Value(false)
    }
}
