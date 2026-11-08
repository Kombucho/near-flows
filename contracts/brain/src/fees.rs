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
//! Both have caps fixed at deployment (at most 10% and 0.1 NEAR): the fee
//! admin may move them within the caps, never above. A cut applies at once; a
//! raise waits FEE_DELAY_NS, visible in `get_fees` as `pending`, so nobody is
//! charged a new price they had no time to see. With no fees at deployment
//! (`fees: null`), they are zero for good.
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
/// The highest signature fee cap a brain may be deployed with: 0.1 NEAR.
pub const MAX_SIGNATURE_FEE_CAP: u128 = 100_000_000_000_000_000_000_000;
/// A fee raise waits this long before it applies: 24 hours.
pub const FEE_DELAY_NS: u64 = 24 * 3_600 * 1_000_000_000;
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

/// A fee raise, waiting for its time (`set_fee`).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingFee {
    pub bps: u16,
    pub per_signature: U128,
    pub active_at_ns: near_sdk::json_types::U64,
}

impl Fees {
    /// No fees, and no way to start charging them.
    pub fn none(treasury: AccountId) -> Self {
        Fees { admin: treasury.clone(), treasury, cap_bps: 0, bps: 0, per_signature: U128(0), per_signature_cap: U128(0) }
    }

    pub fn check(&self) {
        require!(self.cap_bps <= 1_000, "a fee cap over 10% is a typo");
        require!(self.per_signature_cap.0 <= MAX_SIGNATURE_FEE_CAP, "a signature fee cap over 0.1 NEAR is a typo");
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

/// The fees in force now (a raise whose time came included).
pub(crate) fn fees() -> Fees {
    env::state_read::<crate::Brain>().unwrap_or_else(|| env::panic_str("no state")).fees_now()
}

impl crate::Brain {
    /// The fees in force: the stored ones, or a pending raise once its time came.
    pub fn fees_now(&self) -> Fees {
        let mut f = self.fees.clone();
        if let Some(p) = self.pending_fee.as_ref().filter(|p| env::block_timestamp() >= p.active_at_ns.0) {
            f.bps = p.bps;
            f.per_signature = p.per_signature;
        }
        f
    }

    /// The fee admin moves the fees within their caps: a cut at once (dropping any
    /// pending raise), a raise after FEE_DELAY_NS.
    pub(crate) fn move_fees(&mut self, bps: u16, per_signature: U128) {
        require!(env::predecessor_account_id() == self.fees.admin, "fee admin only");
        let now = self.fees_now();
        let mut f = now.clone();
        f.bps = bps;
        f.per_signature = per_signature;
        f.check();
        if bps <= now.bps && per_signature.0 <= now.per_signature.0 {
            self.fees = f;
            self.pending_fee = None;
        } else {
            self.fees = now;
            let at = env::block_timestamp() + FEE_DELAY_NS;
            self.pending_fee = Some(PendingFee { bps, per_signature, active_at_ns: at.into() });
        }
    }
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
                self.spend(fee);
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
        self.spend(fee);
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
        self.spend(fee);
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
    /// beyond what its flows hold (in NEAR: beyond its flows, storage and reserve
    /// too). If the transfer fails, it stays owed. Never during a step: moving a
    /// token between the step's two balance reads would corrupt what it measured.
    pub(crate) fn settle_fees(&mut self, token: AccountId) -> Reply {
        self.idle();
        let owed = self.owed(token.as_str());
        require!(owed > 0, "no fees owed in that token");
        if token.as_str() == NEAR {
            // `free_near` already counts the owed fee as taken: what's there for it is
            // the NEAR beyond the flows' (never the reserve, never a flow's).
            let amount = owed.min(self.near_beyond(self.allocated(NEAR) - owed));
            require!(amount > 0, "the vault holds nothing to settle with yet: add NEAR");
            self.owe_less(NEAR, amount);
            self.spend(amount);
            self.emit("fee", &format!(r#""kind":"settled","token":"near","fee":"{amount}""#));
            return Answer::default().transfer(&fees().treasury, amount).into();
        }
        self.read(vec![self.balance_view(&token)], crate::Read::Settle { token }, crate::GAS_READ)
    }

    pub(crate) fn on_settle(&mut self, token: AccountId) -> Reply {
        self.idle();
        let bal = crate::amount_result(0).unwrap_or_else(|| env::panic_str("couldn't read the vault's balance"));
        let owed = self.owed(token.as_str());
        let flows = crate::allocated(&self.vs, token.as_str()) - owed;
        let amount = owed.min(bal.saturating_sub(flows));
        require!(amount > 0, "the vault holds nothing to settle with yet");
        self.note_token(token.as_str(), true);
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
    #[should_panic(expected = "signature fee cap over 0.1 NEAR")]
    fn a_signature_fee_cap_over_a_tenth_of_a_near_is_refused() {
        let mut f = fees(30, 10);
        f.per_signature_cap = U128(MAX_SIGNATURE_FEE_CAP + 1);
        f.check();
    }

    #[test]
    fn a_fee_raise_waits_a_day_and_a_cut_is_at_once() {
        use near_sdk::test_utils::VMContextBuilder;
        use near_sdk::testing_env;
        let t: AccountId = "treasury.near".parse().unwrap();
        let mut ctx = VMContextBuilder::new();
        testing_env!(ctx.predecessor_account_id(t.clone()).block_timestamp(1_000).build());
        let mut b = crate::Brain::new("flows.near".parse().unwrap(), Some(fees(30, 10)));
        b.move_fees(30, U128(5));
        assert_eq!((b.fees_now().bps, b.fees_now().per_signature.0), (10, 1), "not yet");
        assert_eq!(b.pending_fee.as_ref().unwrap().active_at_ns.0, 1_000 + FEE_DELAY_NS);
        testing_env!(ctx.block_timestamp(1_000 + FEE_DELAY_NS - 1).build());
        assert_eq!(b.fees_now().bps, 10, "a nanosecond early");
        testing_env!(ctx.block_timestamp(1_000 + FEE_DELAY_NS).build());
        assert_eq!((b.fees_now().bps, b.fees_now().per_signature.0), (30, 5), "in force after 24 h");
        // A cut applies at once, from the fees in force, and drops what's pending.
        b.move_fees(30, U128(4));
        b.move_fees(20, U128(5));
        assert_eq!(b.fees_now().bps, 30, "half a cut is still a raise: it waits");
        b.move_fees(0, U128(0));
        assert_eq!((b.fees_now().bps, b.fees_now().per_signature.0, b.pending_fee.is_none()), (0, 0, true));
    }

    #[test]
    fn a_brain_deployed_without_fees_can_never_charge_any() {
        let f = Fees::none("t.near".parse().unwrap());
        assert_eq!((f.bps, f.cap_bps, f.per_signature.0, f.per_signature_cap.0), (0, 0, 0, 0));
    }
}
