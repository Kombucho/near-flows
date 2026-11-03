//! Buying on someone else's behalf, e.g. a relayer or a NEAR Intents
//! withdrawal (`FtWithdraw` with a `msg`): the tokens arrive from a contract
//! that is not the buyer, so the buyer is named in the message (`"for"`).
//!
//! The one rule that matters: a delegated buy must NEVER bounce the tokens
//! back. A refund would go to the sending contract (e.g. intents.near) with no
//! owner attached, and the buyer would lose the money. So on ANY failure
//! (slippage, paused, market full, missing storage) the market is restored
//! exactly as it was and the full amount is recorded as owed to the buyer,
//! who withdraws it with `withdraw_owed`.

use engine::{Rung, DEFAULT_LADDER};
use near_sdk::{env, AccountId};

use crate::{Market, MAX_POSITIONS};

impl Market {
    pub(crate) fn delegated_buy(&mut self, who: &AccountId, amount: u128, min_out: u128, ladder: bool) {
        let before = env::storage_usage();
        let snapshot = self.book.clone(); // restored if anything fails
        let result = self.try_delegated_buy(who, amount, min_out, ladder, before);
        if let Err(reason) = result {
            self.book = snapshot;
            let quote = self.quote.clone();
            self.add_owed(&quote, who, amount);
            self.emit("buy_failed_owed", &format!(r#"{{"account":"{who}","amount":"{amount}","reason":"{reason}"}}"#));
        }
    }

    fn try_delegated_buy(&mut self, who: &AccountId, amount: u128, min_out: u128, ladder: bool, before: u64) -> Result<(), String> {
        let book = self.book.as_ref().ok_or("not launched")?;
        if self.paused {
            return Err("paused".into());
        }
        if book.positions.len() >= MAX_POSITIONS {
            return Err("market full".into());
        }
        let now = env::block_timestamp();
        let rungs: &[Rung] = if ladder { &DEFAULT_LADDER } else { &[] };
        let book = self.book.as_mut().expect("checked above");
        book.touch(now);
        let b = book.buy(who.as_str(), amount, min_out, rungs, now).map_err(|e| format!("{e:?}"))?;
        if !self.try_charge_storage(who, before) {
            return Err("beneficiary has no storage balance on the market".into());
        }
        // Only after every check passed: unspent input is owed back to the
        // buyer (not refunded to the sender). Recording it earlier would count
        // it twice when a later check fails and the full amount is owed.
        if b.swap.refund > 0 {
            let quote = self.quote.clone();
            self.add_owed(&quote, who, b.swap.refund);
        }
        let token = self.token.clone();
        self.pay(&token, who, b.kept);
        self.emit("buy", &format!(r#"{{"account":"{who}","in":"{}","out":"{}","ladder":{},"locked":{},"delegated":true}}"#,
            b.swap.used, b.swap.out, b.ladder.len(), b.locked));
        Ok(())
    }
}
