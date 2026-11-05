//! IDEAS ("pre-IPO"): backers fund a project; the seller is paid only after
//! delivering what was pledged, and only by the backers who accept it.
//!
//!   1. FUNDING. The seller pledges `amount` tokens (for all shares) by
//!      `deliver_by_ns`. Buyers' money is held here, not paid to the seller.
//!   2. DELIVERY = the proof. The seller sends the pledged tokens (pro rata
//!      to the shares actually sold) with {"action":"deliver","lot":ID}. The
//!      contract checks the amount; it cannot tell a real coin from a fake
//!      one, so a REVIEW of 3 days follows.
//!   3. REVIEW. Each backer decides for THEMSELVES: do nothing (or `accept`)
//!      and take their share of the tokens, or `reject` and take their own
//!      money back. Nobody can overrule anyone: there is no vote to buy with
//!      fake backer accounts. After the review the seller is paid what the
//!      accepting backers paid (minus the 5% fee), and gets back the tokens
//!      of those who rejected.
//!   4. No delivery by the deadline (or the seller cancels): REFUNDING.
//! Claims (`claim`) can be triggered by anyone and only ever pay the buyer.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId};

use crate::{Bazaar, BazaarExt, Item, Sale, HOUR_NS};

/// Default review window (the contract's `review_ns` can be set at deploy).
pub const REVIEW_NS: u64 = 72 * HOUR_NS;
const DAY_NS: u64 = 24 * HOUR_NS;

/// What the seller promises: `amount` tokens for ALL shares, by `deliver_by_ns`.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pledge {
    pub amount: U128,
    pub deliver_by_ns: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Funding,
    Review,
    Released,
    Refunding,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Escrow {
    pub raised: u128,
    pub sold: u128, // shares sold
    /// The token delivered and the amount held for buyers.
    pub delivered: Option<(AccountId, u128)>,
    pub review_ends_ns: u64,
    /// What the rejecting backers paid, and their shares.
    pub rejected_paid: u128,
    pub rejected_shares: u128,
    pub phase: Phase,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default)]
pub struct Stake {
    pub paid: u128,
    pub shares: u128,
    pub rejected: bool,
    pub claimed: bool,
}

#[near(serializers = [json])]
pub struct EscrowView {
    pub phase: Phase,
    pub raised: U128,
    pub sold: U128,
    pub delivered_token: Option<AccountId>,
    pub delivered_amount: Option<U128>,
    pub review_ends_ns: u64,
    pub rejected_paid: U128,
    pub deliver_by_ns: u64,
    pub pledged: U128,
}

#[near(serializers = [json])]
pub struct StakeView {
    pub paid: U128,
    pub shares: U128,
    pub rejected: bool,
    pub claimed: bool,
}

fn stake_key(lot: u64, who: &AccountId) -> String {
    format!("{lot}|{who}")
}

#[near]
impl Bazaar {
    /// Backer, during the review: "I don't want it". You'll claim back what
    /// you paid instead of the tokens. Only your own stake is affected.
    pub fn reject(&mut self, lot_id: u64) {
        let who = env::predecessor_account_id();
        let mut e = self.escrow(lot_id);
        require!(e.phase == Phase::Review && env::block_timestamp() < e.review_ends_ns, "not in review");
        let key = stake_key(lot_id, &who);
        let mut s = self.stakes.get(&key).cloned().unwrap_or_else(|| env::panic_str("you hold no stake"));
        require!(!s.rejected, "already rejected");
        s.rejected = true;
        e.rejected_paid += s.paid;
        e.rejected_shares += s.shares;
        self.stakes.insert(key, s);
        self.escrows.insert(lot_id, e);
        self.emit("reject", &format!(r#"{{"lot":{lot_id},"backer":"{who}"}}"#));
    }

    /// Anyone, after the review: pay the seller for the accepted shares and
    /// hand back the tokens of the rejected ones.
    pub fn finalize(&mut self, lot_id: u64) {
        let mut e = self.escrow(lot_id);
        require!(e.phase == Phase::Review, "not in review");
        require!(env::block_timestamp() >= e.review_ends_ns, "review still running");
        let lot = self.lot(lot_id);
        let (token, amount) = e.delivered.clone().expect("delivered in review");
        e.phase = Phase::Released;
        let accepted_paid = e.raised - e.rejected_paid;
        let tokens_back = math::wide::mul_div_down(amount, e.rejected_shares, e.sold);
        let (rejected, sold) = (e.rejected_shares, e.sold);
        self.escrows.insert(lot_id, e);
        self.pay_seller(&lot.pay, &lot.seller, accepted_paid);
        self.pay(&Some(token), &lot.seller, tokens_back);
        self.emit("finalize", &format!(r#"{{"lot":{lot_id},"paid_to_seller":"{accepted_paid}","rejected_shares":"{rejected}","shares":"{sold}"}}"#));
    }

    /// Anyone, after the deadline with no delivery: refunds open for buyers.
    pub fn expire(&mut self, lot_id: u64) {
        let lot = self.lot(lot_id);
        let e = self.escrow(lot_id);
        let by = lot.pledge.as_ref().expect("idea").deliver_by_ns;
        require!(e.phase == Phase::Funding && env::block_timestamp() >= by, "not expired");
        self.abandon(lot_id);
    }

    /// Anyone may trigger it; it pays only `account`. Released: their share
    /// of the delivered tokens, or (if they rejected) what they paid.
    /// Refunding: what they paid.
    pub fn claim(&mut self, lot_id: u64, account: AccountId) {
        let e = self.escrow(lot_id);
        let key = stake_key(lot_id, &account);
        let mut s = self.stakes.get(&key).cloned().unwrap_or_else(|| env::panic_str("no stake"));
        require!(!s.claimed, "already claimed");
        s.claimed = true;
        self.stakes.insert(key, s.clone());
        match e.phase {
            Phase::Released if s.rejected => {
                let lot = self.lot(lot_id);
                self.pay(&lot.pay, &account, s.paid);
            }
            Phase::Released => {
                let (token, amount) = e.delivered.clone().expect("delivered");
                let out = math::wide::mul_div_down(amount, s.shares, e.sold);
                self.pay(&Some(token), &account, out);
            }
            Phase::Refunding => {
                let lot = self.lot(lot_id);
                self.pay(&lot.pay, &account, s.paid);
            }
            _ => env::panic_str("nothing to claim yet"),
        }
        self.emit("claim", &format!(r#"{{"lot":{lot_id},"buyer":"{account}"}}"#));
    }

    pub fn get_escrow(&self, lot_id: u64) -> Option<EscrowView> {
        let e = self.escrows.get(&lot_id)?;
        let p = self.lots.get(&lot_id)?.pledge.clone()?;
        Some(EscrowView {
            phase: e.phase, raised: U128(e.raised), sold: U128(e.sold),
            delivered_token: e.delivered.as_ref().map(|d| d.0.clone()), delivered_amount: e.delivered.as_ref().map(|d| U128(d.1)),
            review_ends_ns: e.review_ends_ns, rejected_paid: U128(e.rejected_paid), deliver_by_ns: p.deliver_by_ns, pledged: p.amount,
        })
    }

    pub fn get_stake(&self, lot_id: u64, account_id: AccountId) -> Option<StakeView> {
        let s = self.stakes.get(&stake_key(lot_id, &account_id))?;
        Some(StakeView { paid: U128(s.paid), shares: U128(s.shares), rejected: s.rejected, claimed: s.claimed })
    }
}

impl Bazaar {
    pub(crate) fn escrow(&self, lot_id: u64) -> Escrow {
        self.escrows.get(&lot_id).cloned().unwrap_or_else(|| env::panic_str("no escrow for this lot"))
    }

    /// Set an idea's pledge (at creation) and open its escrow.
    pub(crate) fn attach_pledge(&mut self, lot_id: u64, pledge: Pledge) {
        let now = env::block_timestamp();
        let mut lot = self.lot(lot_id);
        require!(pledge.amount.0 > 0, "pledge some tokens");
        let earliest = match &lot.sale { Sale::Listing { falling: Some(f), .. } => f.end_ns, _ => now } + DAY_NS;
        require!(pledge.deliver_by_ns >= earliest, "deadline: at least a day after the sale can end");
        require!(pledge.deliver_by_ns <= now + 365 * DAY_NS, "deadline: within a year");
        lot.pledge = Some(pledge);
        self.lots.insert(lot_id, lot);
        self.escrows.insert(lot_id, Escrow { raised: 0, sold: 0, delivered: None, review_ends_ns: 0, rejected_paid: 0, rejected_shares: 0, phase: Phase::Funding });
    }

    /// Sales of an idea are only allowed while it is funding.
    pub(crate) fn assert_funding(&self, lot_id: u64) {
        let lot = self.lot(lot_id);
        let e = self.escrow(lot_id);
        let by = lot.pledge.as_ref().expect("idea").deliver_by_ns;
        require!(e.phase == Phase::Funding && env::block_timestamp() < by, "this idea is no longer funding");
    }

    /// Record a buyer's money and shares (held, not paid to the seller).
    pub(crate) fn escrow_add(&mut self, lot_id: u64, who: &AccountId, paid: u128, shares: u128) {
        let mut e = self.escrow(lot_id);
        e.raised += paid;
        e.sold += shares;
        self.escrows.insert(lot_id, e);
        let key = stake_key(lot_id, who);
        let mut s = self.stakes.get(&key).cloned().unwrap_or_default();
        s.paid += paid;
        s.shares += shares;
        self.stakes.insert(key, s);
    }

    /// The seller cancelled, or the deadline passed: buyers get refunds.
    pub(crate) fn abandon(&mut self, lot_id: u64) {
        let mut e = self.escrow(lot_id);
        require!(e.phase == Phase::Funding, "too late to abandon");
        e.phase = Phase::Refunding;
        self.escrows.insert(lot_id, e);
        let mut lot = self.lot(lot_id);
        if let Sale::Listing { open, .. } = &mut lot.sale {
            *open = false;
        }
        self.lots.insert(lot_id, lot);
        self.emit("refunding", &format!(r#"{{"lot":{lot_id}}}"#));
    }

    /// The seller delivers `amount` of `token`. Returns the excess to refund.
    pub(crate) fn deliver(&mut self, lot_id: u64, seller: &AccountId, token: AccountId, amount: u128) -> u128 {
        let mut lot = self.lot(lot_id);
        require!(&lot.seller == seller, "only the seller delivers");
        require!(matches!(lot.item, Item::Idea { .. }), "not an idea");
        let mut e = self.escrow(lot_id);
        let pledge = lot.pledge.clone().expect("idea");
        require!(e.phase == Phase::Funding && env::block_timestamp() < pledge.deliver_by_ns, "not open for delivery");
        require!(e.sold > 0, "nobody has bought yet");
        // Pledge pro rata to the shares actually sold, rounded up.
        let need = math::wide::mul_div_up(pledge.amount.0, e.sold, crate::units_of(&lot.item));
        require!(amount >= need, "less than pledged");
        if let Sale::Listing { open, .. } = &mut lot.sale {
            *open = false; // no more sales once delivered
        }
        self.lots.insert(lot_id, lot);
        e.delivered = Some((token.clone(), need));
        e.phase = Phase::Review;
        e.review_ends_ns = env::block_timestamp() + self.review_ns;
        self.escrows.insert(lot_id, e);
        self.emit("deliver", &format!(r#"{{"lot":{lot_id},"token":"{token}","amount":"{need}"}}"#));
        amount - need
    }
}
