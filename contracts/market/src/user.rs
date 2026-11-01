//! User actions that do not bring tokens in: claim, withdraw, unstake, vote,
//! and the public crank. Each one updates the engine first, then pays out.
//!
//! `#[payable]` + `assert_one_yocto()`: the caller must attach exactly
//! 1 yoctoNEAR. Only a full-access key can attach a deposit, so a leaked
//! limited "app key" cannot be used to move a user's money.

use engine::Vote;
use near_sdk::json_types::U128;
use near_sdk::{assert_one_yocto, env, near, require};

use crate::{Market, MarketExt};

#[near]
impl Market {
    /// Claim a position's earnings (LP fees, take-profit proceeds, creator slice).
    #[payable]
    pub fn claim(&mut self, position_id: u64) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let (q, t) = self.book().claim(position_id, who.as_str()).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.pay_both(&who, q, t);
        self.emit("claim", &format!(r#"{{"account":"{who}","position":{position_id},"quote":"{q}","token":"{t}"}}"#));
    }

    /// Close a position (not while locked). Works even when paused.
    #[payable]
    pub fn withdraw(&mut self, position_id: u64) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let before = env::storage_usage();
        let now = env::block_timestamp();
        let w = self.book().withdraw(position_id, who.as_str(), now).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.pay_both(&who, w.quote + w.fee_quote, w.token + w.fee_token);
        self.charge_storage(&who, before); // freed bytes are credited back
        self.emit("withdraw", &format!(r#"{{"account":"{who}","position":{position_id}}}"#));
    }

    #[payable]
    pub fn claim_dividends(&mut self) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let (q, t) = self.book().claim_dividends(who.as_str()).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.pay_both(&who, q, t);
    }

    /// Change your standing vote: [dividend, burn, deepen] in bps, summing to 10_000.
    pub fn set_vote(&mut self, vote: Vote) {
        let who = env::predecessor_account_id();
        self.book().set_vote(who.as_str(), vote).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.emit("vote", &format!(r#"{{"account":"{who}","vote":[{},{},{}]}}"#, vote[0], vote[1], vote[2]));
    }

    /// Start unstaking; the tokens can be withdrawn after the 7-day delay.
    #[payable]
    pub fn request_unstake(&mut self, amount: U128) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let now = env::block_timestamp();
        self.book().request_unstake(who.as_str(), amount.0, now).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
    }

    #[payable]
    pub fn withdraw_unstaked(&mut self) {
        assert_one_yocto();
        let who = env::predecessor_account_id();
        let now = env::block_timestamp();
        let amount = self.book().withdraw_unstaked(who.as_str(), now).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let token = self.token.clone();
        self.pay(&token, &who, amount);
    }

    /// Public and permissionless: anyone may call it, at most once an hour.
    /// Pays dividends, burns and deepens by the fixed rules. Paused = waits.
    pub fn crank(&mut self) {
        require!(!self.paused, "paused");
        let now = env::block_timestamp();
        let r = self.book().crank(now).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.emit("crank", &format!(r#"{{"dividend_quote":"{}","dividend_token":"{}","burned":"{}"}}"#,
            r.dividend_q, r.dividend_t, r.burned));
    }
}
