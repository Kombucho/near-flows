//! Receiving tokens. On NEAR you "call" a contract with tokens by running
//! `ft_transfer_call` on the TOKEN contract; the token moves the funds and then
//! calls our `ft_on_transfer` with a `msg` saying what to do with them.
//! Whatever amount we return is refunded to the sender by the token contract.
//!
//! msg examples (JSON):
//!   {"action":"buy","min_out":"1000","ladder":true}   pay wNEAR, get the coin (+ exit plan)
//!   {"action":"buy","min_out":"1000","for":"alice.near"}  buy on behalf of alice (relayers,
//!        NEAR Intents withdrawals): NEVER refunds; any failure is recorded as owed to alice
//!   {"action":"sell","min_out":"1000"}                pay the coin, get wNEAR
//!   {"action":"ladder"}                               put coins you hold on a take-profit ladder
//!   {"action":"stake"}                                stake coins (native coins only)
//!   {"action":"lp","bin":12,"lock_days":30}           add liquidity to one bin
//!   {"action":"fund"}                                 owner, before launch

use engine::{Rung, Side, DEFAULT_LADDER};
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::serde::Deserialize;
use near_sdk::{env, near, require, AccountId, PromiseOrValue};

use crate::{Market, MarketExt, DAY_NS, MAX_POSITIONS};

/// A ladder for coins you already hold: all of them, 25% on each rung.
const FULL_LADDER: [Rung; 4] = [(15_000, 2_500), (20_000, 2_500), (30_000, 2_500), (50_000, 2_500)];

#[derive(Deserialize)]
#[serde(crate = "near_sdk::serde", tag = "action", rename_all = "snake_case")]
enum Msg {
    Buy {
        min_out: U128,
        #[serde(default)]
        ladder: bool,
        /// Beneficiary: who gets the coins and rungs. Set by relayers and by
        /// NEAR Intents withdrawals, where the sender is not the buyer.
        #[serde(default, rename = "for")]
        beneficiary: Option<AccountId>,
    },
    Sell { min_out: U128 },
    Ladder,
    Stake,
    Lp { bin: i32, #[serde(default)] lock_days: u32 },
    Fund,
}

#[near]
impl FungibleTokenReceiver for Market {
    fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        // Only our two tokens may call this. Anyone can deploy a fake token
        // that "sends" fake coins; this check is what stops it.
        let from_token = env::predecessor_account_id();
        let is_quote = from_token == self.quote;
        require!(is_quote || from_token == self.token, "unknown token");
        let msg: Msg = near_sdk::serde_json::from_str(&msg).unwrap_or_else(|_| env::panic_str("bad msg"));
        if let Msg::Buy { min_out, ladder, beneficiary: Some(who) } = &msg {
            if is_quote {
                // Delegated buy: never panics from here on (see delegated.rs).
                self.delegated_buy(who, amount.0, min_out.0, *ladder);
                return PromiseOrValue::Value(U128(0));
            }
        }
        let before = env::storage_usage();
        let refund = match msg {
            Msg::Fund => self.on_fund(&sender_id, is_quote, amount.0),
            other => {
                require!(!self.paused, "paused");
                self.on_action(&sender_id, is_quote, amount.0, other)
            }
        };
        self.charge_storage(&sender_id, before);
        PromiseOrValue::Value(U128(refund))
    }
}

impl Market {
    fn on_fund(&mut self, sender: &AccountId, is_quote: bool, amount: u128) -> u128 {
        require!(*sender == self.owner, "owner only");
        require!(self.book.is_none(), "already launched");
        if is_quote { self.funded_quote += amount } else { self.funded_token += amount }
        0
    }

    /// Run one action. Returns the amount to refund to the sender.
    fn on_action(&mut self, sender: &AccountId, is_quote: bool, amount: u128, msg: Msg) -> u128 {
        let now = env::block_timestamp();
        let who = sender.to_string();
        require!(self.book_ref().positions.len() < MAX_POSITIONS, "market full");
        self.book().touch(now); // decay the fee's volatility memory up to now
        match msg {
            Msg::Buy { min_out, ladder, .. } => {
                require!(is_quote, "pay with the quote token to buy");
                let rungs: &[Rung] = if ladder { &DEFAULT_LADDER } else { &[] };
                let b = self.book().buy(&who, amount, min_out.0, rungs, now).unwrap_or_else(|e| fail(e));
                let token = self.token.clone();
                self.pay(&token, sender, b.kept);
                self.emit("buy", &format!(r#"{{"account":"{who}","in":"{}","out":"{}","ladder":{},"locked":{}}}"#,
                    b.swap.used, b.swap.out, b.ladder.len(), b.locked));
                b.swap.refund
            }
            Msg::Sell { min_out } => {
                require!(!is_quote, "send the coin to sell");
                let r = self.book().swap(Side::Sell, amount, min_out.0).unwrap_or_else(|e| fail(e));
                let quote = self.quote.clone();
                self.pay(&quote, sender, r.out);
                self.emit("sell", &format!(r#"{{"account":"{who}","in":"{}","out":"{}"}}"#, r.used, r.out));
                r.refund
            }
            Msg::Ladder => {
                require!(!is_quote, "send the coin to ladder it");
                let (ids, placed) = self.book().place_ladder(&who, amount, &FULL_LADDER, 0, now).unwrap_or_else(|e| fail(e));
                self.emit("ladder", &format!(r#"{{"account":"{who}","placed":"{placed}","rungs":{}}}"#, ids.len()));
                amount - placed
            }
            Msg::Stake => {
                require!(!is_quote, "stake the coin");
                self.book().stake(&who, amount).unwrap_or_else(|e| fail(e));
                self.emit("stake", &format!(r#"{{"account":"{who}","amount":"{amount}"}}"#));
                0
            }
            Msg::Lp { bin, lock_days } => {
                let lock = if lock_days == 0 { 0 } else { now + lock_days as u64 * DAY_NS };
                let (t, q) = if is_quote { (0, amount) } else { (amount, 0) };
                let id = self.book().add_liquidity(&who, bin, t, q, lock, now).unwrap_or_else(|e| fail(e));
                self.emit("lp", &format!(r#"{{"account":"{who}","position":{id},"bin":{bin}}}"#));
                0
            }
            Msg::Fund => unreachable!("handled above"),
        }
    }
}

/// Engine errors abort the call. On NEAR that reverts every change made in
/// this call, and the token contract refunds the full amount to the sender.
fn fail(e: engine::Error) -> ! {
    env::panic_str(&format!("{e:?}"))
}
