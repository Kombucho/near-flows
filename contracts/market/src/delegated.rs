//! Buying on someone else's behalf, e.g. a relayer or a NEAR Intents
//! withdrawal (`FtWithdraw` with a `msg`): the tokens arrive from a contract
//! that is not the buyer, so the buyer is named in the message (`"for"`).
//!
//! The one rule that matters: a delegated buy must NEVER bounce the tokens
//! back. A refund would go to the sending contract (e.g. intents.near) with no
//! owner attached, and the buyer would lose the money.
//!
//! So `ft_on_transfer` keeps the tokens and runs the buy as its OWN receipt
//! (`delegated_buy_inner`), followed by a callback. If the buy fails for any
//! reason (slippage, paused, missing storage...) NEAR reverts everything that
//! receipt did, and the callback records the full amount as owed to the buyer,
//! who withdraws it with `withdraw_owed`.

use engine::{Rung, DEFAULT_LADDER};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, PromiseError};

use crate::{Market, MarketExt};

const GAS_INNER: Gas = Gas::from_tgas(60);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);

impl Market {
    pub(crate) fn delegated_buy(&mut self, who: &AccountId, amount: u128, min_out: u128, ladder: bool) {
        let me = env::current_account_id();
        Self::ext(me.clone()).with_static_gas(GAS_INNER)
            .delegated_buy_inner(who.clone(), U128(amount), U128(min_out), ladder)
            .then(Self::ext(me).with_static_gas(GAS_CALLBACK).on_delegated_buy(who.clone(), U128(amount)))
            .detach();
    }
}

#[near]
impl Market {
    /// The delegated buy itself. `#[private]`: only this contract may call it
    /// (the tokens are already here). Any panic reverts this receipt only.
    #[private]
    pub fn delegated_buy_inner(&mut self, who: AccountId, amount: U128, min_out: U128, ladder: bool) {
        require!(!self.paused, "paused");
        let before = env::storage_usage();
        let now = env::block_timestamp();
        let rungs: &[Rung] = if ladder { &DEFAULT_LADDER } else { &[] };
        let book = self.book();
        book.touch(now);
        let b = book.buy(who.as_str(), amount.0, min_out.0, rungs, now).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.charge_storage(&who, before);
        // Unspent input is owed back to the buyer, never refunded to the sender.
        if b.swap.refund > 0 {
            let quote = self.quote.clone();
            self.add_owed(&quote, &who, b.swap.refund);
        }
        let token = self.token.clone();
        self.pay(&token, &who, b.kept);
        self.emit("buy", &format!(r#"{{"account":"{who}","in":"{}","out":"{}","ladder":{},"locked":{},"delegated":true}}"#,
            b.swap.used, b.swap.out, b.ladder.len(), b.locked));
    }

    /// After the delegated buy: if it failed, the whole amount is owed to the
    /// buyer. Never panics.
    #[private]
    pub fn on_delegated_buy(&mut self, who: AccountId, amount: U128, #[callback_result] result: Result<(), PromiseError>) {
        if result.is_err() {
            let quote = self.quote.clone();
            self.add_owed(&quote, &who, amount.0);
            self.emit("buy_failed_owed", &format!(r#"{{"account":"{who}","amount":"{}"}}"#, amount.0));
        }
    }
}
