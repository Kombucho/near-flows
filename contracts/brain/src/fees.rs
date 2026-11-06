//! Fees, enforced by the brain for every vault on it (own keepers included).
//!
//! PROTOCOL FEE on NEAR-side steps: `bps` of the input a successful step
//! actually used (as measured, never on a failed or refunded step), in the
//! input token. NEAR goes to the treasury in the step's final answer (a plain
//! transfer: no gas to speak of). A token fee is set aside in the vault's
//! record (`owed_fees`), out of its free balance at once, and anyone may
//! `settle_fees` it to the treasury later. WHY not ft_transfer it inside the
//! step: learning whether that transfer worked (the treasury may not be
//! registered with the token) takes one more round per step, ~21 Tgas, and the
//! node library's Borrow on Rhea would no longer fit in 300 Tgas. A settle that
//! fails puts the fee back in `owed_fees`: the user is never blocked.
//!
//! FLAT FEE per cross-chain signature (evm_call, sol_sign_*, intents_sign_swap,
//! step_remote): `per_signature` NEAR from the vault's free NEAR, sent with the
//! signature request; refused up front if the vault can't pay it.
//!
//! Both have caps fixed at deployment: the fee admin may move them within
//! the caps, never above. With no fees at deployment (`fees: null`), they are
//! zero for good.
//!
//! KEEPER FEE: the owner names the keeper and its price together
//! (`set_keeper`, at most MAX_KEEPER_FEE). After each step or remote report
//! the keeper ran successfully, the vault pays it that much NEAR. Only the
//! owner can change it: a keeper raising its price needs the owner's new
//! `set_keeper`. No free work: a keeper step or remote step on a vault whose
//! free NEAR can't cover the price is refused UP FRONT (before anything moves,
//! the vault never left busy), pointing the owner to running their own keeper
//! for free (REPO_URL, AGENTS.md). The owner can always run it themselves.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use crate::wire::Answer;
use crate::{Next, Reply, Talk, NEAR};

/// Where to learn to run your own keeper (fee 0). Placeholder until the owner confirms the repo.
pub const REPO_URL: &str = "https://github.com/Kombucho/near-flows";

/// A fat-finger guard on the keeper's price: 0.05 NEAR per run.
pub const MAX_KEEPER_FEE: u128 = 50_000_000_000_000_000_000_000;
/// The fee's ft_transfer when settling (mock and real NEP-141 transfers burn 2-4 Tgas).
const GAS_FT_FEE: Gas = Gas::from_tgas(10);
/// The settle's callback: only answers (burns ~2.5 Tgas).
pub(crate) const GAS_SETTLED: Gas = Gas::from_tgas(8);

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Fees {
    pub treasury: AccountId,
    pub admin: AccountId,
    pub cap_bps: u16,
    pub bps: u16,
    pub per_signature: U128,
    pub per_signature_cap: U128,
}

impl Fees {
    /// No fees, and no way to start charging them.
    pub fn none(treasury: AccountId) -> Self {
        Fees { admin: treasury.clone(), treasury, cap_bps: 0, bps: 0, per_signature: U128(0), per_signature_cap: U128(0) }
    }

    pub fn check(&self) {
        require!(self.cap_bps <= 1_000, "a fee cap over 10% is a typo");
        require!(self.bps <= self.cap_bps, "fee over its cap");
        require!(self.per_signature.0 <= self.per_signature_cap.0, "signature fee over its cap");
    }

    /// The protocol's share of `used`, rounded down.
    pub fn of(&self, used: u128) -> u128 {
        crate::mul_div(used, self.bps as u128, 10_000)
    }
}

/// The refusal a keeper gets on a vault that can't pay it (the keeper script logs the same pointer).
pub fn keeper_unpaid(fee: u128, free: u128) -> String {
    format!("keeper fee unpaid: the keeper's price is {fee} yoctoNEAR and the vault's free NEAR is {free}: top up NEAR, or run your own keeper for free: {REPO_URL} (AGENTS.md)")
}

pub(crate) fn fees() -> Fees {
    env::state_read::<crate::Brain>().unwrap_or_else(|| env::panic_str("no state")).fees
}

impl Talk {
    /// A step used `used` of `token`: the protocol's fee, and the keeper's if it ran it.
    /// Returns the answer with the NEAR transfers added.
    pub(crate) fn step_fees(&mut self, ans: Answer, token: &str, used: u128, by_keeper: bool) -> Answer {
        let f = fees();
        let fee = f.of(used);
        let mut ans = ans;
        if fee > 0 {
            if token == NEAR && self.free_near() >= fee {
                ans = ans.transfer(&f.treasury, fee);
            } else {
                self.owe(token, fee);
            }
            self.emit("fee", &format!(r#""kind":"protocol","token":"{token}","used":"{used}","fee":"{fee}","bps":{}"#, f.bps));
        }
        if by_keeper {
            ans = self.pay_keeper(ans);
        }
        ans
    }

    /// Before a keeper run (not the owner's): refuse it if the vault can't pay the
    /// keeper's price. Called before anything moves, so a refusal leaves no trace.
    pub(crate) fn require_keeper_paid(&self, by_owner: bool) {
        let fee = self.vs.keeper_fee.0;
        if by_owner || fee == 0 || self.vs.keeper.is_none() {
            return;
        }
        let free = self.free_near();
        if free < fee {
            env::panic_str(&keeper_unpaid(fee, free));
        }
    }

    /// The keeper's price for a run it did. Checked up front (`require_keeper_paid`);
    /// only if the free NEAR left meanwhile (the owner withdrew while a remote step
    /// was out) does it go unpaid, logged.
    pub(crate) fn pay_keeper(&mut self, ans: Answer) -> Answer {
        let (Some(k), fee) = (self.vs.keeper.clone(), self.vs.keeper_fee.0) else { return ans };
        if fee == 0 {
            return ans;
        }
        if self.free_near() < fee {
            self.emit("fee", &format!(r#""kind":"keeper_unpaid","keeper":"{k}","fee":"{fee}""#));
            return ans;
        }
        self.emit("fee", &format!(r#""kind":"keeper","keeper":"{k}","fee":"{fee}""#));
        ans.transfer(&k, fee)
    }

    /// The flat signature fee, sent with the signature request; refused if the vault can't pay.
    pub(crate) fn charge_signature(&mut self, ans: Answer) -> Answer {
        let f = fees();
        let fee = f.per_signature.0;
        if fee == 0 {
            return ans;
        }
        require!(self.free_near() >= fee, format!("refused: the signature fee is {fee} yoctoNEAR and the vault's free NEAR is {}: add NEAR", self.free_near()));
        self.emit("fee", &format!(r#""kind":"signature","fee":"{fee}""#));
        ans.transfer(&f.treasury, fee)
    }

    pub(crate) fn owe(&mut self, token: &str, amount: u128) {
        match self.vs.owed_fees.iter_mut().find(|(t, _)| t == token) {
            Some((_, a)) => a.0 += amount,
            None => self.vs.owed_fees.push((token.to_string(), U128(amount))),
        }
    }

    fn owe_less(&mut self, token: &str, amount: u128) {
        for (t, a) in self.vs.owed_fees.iter_mut() {
            if t == token {
                a.0 -= amount;
            }
        }
        self.vs.owed_fees.retain(|(_, a)| a.0 > 0);
    }

    pub(crate) fn owed(&self, token: &str) -> u128 {
        self.vs.owed_fees.iter().find(|(t, _)| t == token).map_or(0, |(_, a)| a.0)
    }

    /// Anyone: send a token's owed fees to the treasury, as much as the vault holds
    /// beyond what its flows hold. If the transfer fails, it stays owed.
    pub(crate) fn settle_fees(&mut self, token: AccountId) -> Reply {
        let owed = self.owed(token.as_str());
        require!(owed > 0, "no fees owed in that token");
        if token.as_str() == NEAR {
            let amount = owed.min(self.free_near() + owed);
            self.owe_less(NEAR, amount);
            self.emit("fee", &format!(r#""kind":"settled","token":"near","fee":"{amount}""#));
            return Answer::default().transfer(&fees().treasury, amount).into();
        }
        self.read(vec![self.balance_view(&token)], crate::Read::Settle { token }, crate::GAS_READ)
    }

    pub(crate) fn on_settle(&mut self, token: AccountId) -> Reply {
        let bal = crate::amount_result(0).unwrap_or_else(|| env::panic_str("couldn't read the vault's balance"));
        let owed = self.owed(token.as_str());
        let flows = crate::allocated(&self.vs, token.as_str()) - owed;
        let amount = owed.min(bal.saturating_sub(flows));
        require!(amount > 0, "the vault holds nothing to settle with yet");
        self.owe_less(token.as_str(), amount);
        let treasury = fees().treasury;
        let args = near_sdk::serde_json::json!({"receiver_id": treasury, "amount": amount.to_string()}).to_string();
        let next = near_sdk::borsh::to_vec(&Next::FeeSettled { token: token.to_string(), amount }).unwrap();
        Answer::default().call(&token, "ft_transfer", args.as_bytes(), 1, GAS_FT_FEE).then("on_vault", next, GAS_SETTLED).into()
    }

    pub(crate) fn on_fee_settled(&mut self, token: String, amount: u128) -> Reply {
        if crate::result(0, 64).is_some() {
            self.emit("fee", &format!(r#""kind":"settled","token":"{token}","fee":"{amount}""#));
        } else {
            self.owe(&token, amount);
            self.emit("fee", &format!(r#""kind":"settle_failed","token":"{token}","fee":"{amount}""#));
        }
        Answer::default().into()
    }

    pub(crate) fn set_keeper(&mut self, keeper: Option<AccountId>, fee: U128) {
        require!(fee.0 <= MAX_KEEPER_FEE, "keeper fee: at most 0.05 NEAR per run");
        require!(keeper.is_some() || fee.0 == 0, "no keeper, no keeper fee");
        self.vs.keeper = keeper;
        self.vs.keeper_fee = fee;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fees(cap_bps: u16, bps: u16) -> Fees {
        let t: AccountId = "treasury.near".parse().unwrap();
        Fees { treasury: t.clone(), admin: t, cap_bps, bps, per_signature: U128(1), per_signature_cap: U128(5) }
    }

    #[test]
    fn the_fee_is_bps_of_what_was_used_rounded_down() {
        assert_eq!(fees(30, 10).of(123_456_789), 123_456);
        assert_eq!(fees(30, 10).of(999), 0, "under one unit: nothing");
        assert_eq!(fees(30, 30).of(10u128.pow(30)), 3 * 10u128.pow(27));
    }

    #[test]
    #[should_panic(expected = "fee over its cap")]
    fn a_fee_over_its_cap_is_refused() {
        fees(30, 31).check();
    }

    #[test]
    fn the_unpaid_keeper_refusal_points_to_running_your_own() {
        let m = keeper_unpaid(10, 3);
        assert!(m.starts_with("keeper fee unpaid") && m.contains("top up NEAR") && m.contains(REPO_URL) && m.contains("AGENTS.md"), "{m}");
    }

    #[test]
    fn a_brain_deployed_without_fees_can_never_charge_any() {
        let f = Fees::none("t.near".parse().unwrap());
        assert_eq!((f.bps, f.cap_bps, f.per_signature.0, f.per_signature_cap.0), (0, 0, 0, 0));
    }
}
