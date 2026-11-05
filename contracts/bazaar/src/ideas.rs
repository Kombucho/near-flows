//! IDEAS ("pre-IPO"): buyers fund a project; the seller is paid only after
//! delivering what was pledged, and only if buyers accept it.
//!
//!   1. FUNDING. The seller pledges `amount` tokens (for all shares) by
//!      `deliver_by_ns`. Buyers' money is held here, not paid to the seller.
//!   2. DELIVERY = the proof. The seller sends the pledged tokens (pro rata
//!      to the shares actually sold) with {"action":"deliver","lot":ID}. The
//!      contract checks the amount; it cannot tell a real coin from a fake
//!      one, so a REVIEW of 3 days follows.
//!   3. REVIEW. Each buyer may object, weighted by what they paid.
//!      Under 50% objecting: RELEASED. The seller gets the money, buyers
//!      claim their tokens. 50% or more: REFUNDING. Buyers get their money
//!      back, the seller gets the tokens back.
//!   4. No delivery by the deadline (or the seller cancels): REFUNDING.
//! Claims (`claim`) can be triggered by anyone and only ever pay the buyer.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId};

use crate::{Bazaar, BazaarExt, Item, Sale, HOUR_NS};

pub const REVIEW_NS: u64 = 72 * HOUR_NS;
/// Objections (weighted by money paid) at or above this share fail a delivery.
pub const OBJECT_BPS: u128 = 5_000;
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
    pub objections: u128,
    pub phase: Phase,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default)]
pub struct Stake {
    pub paid: u128,
    pub shares: u128,
    pub objected: bool,
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
    pub objections: U128,
    pub deliver_by_ns: u64,
    pub pledged: U128,
}

#[near(serializers = [json])]
pub struct StakeView {
    pub paid: U128,
    pub shares: U128,
    pub objected: bool,
    pub claimed: bool,
}

fn stake_key(lot: u64, who: &AccountId) -> String {
    format!("{lot}|{who}")
}

#[near]
impl Bazaar {
    /// Buyer: object to a delivery during its review.
    pub fn object(&mut self, lot_id: u64) {
        let who = env::predecessor_account_id();
        let mut e = self.escrow(lot_id);
        require!(e.phase == Phase::Review && env::block_timestamp() < e.review_ends_ns, "not in review");
        let key = stake_key(lot_id, &who);
        let mut s = self.stakes.get(&key).cloned().unwrap_or_else(|| env::panic_str("you hold no stake"));
        require!(!s.objected, "already objected");
        s.objected = true;
        e.objections += s.paid;
        self.stakes.insert(key, s);
        self.escrows.insert(lot_id, e);
        self.emit("object", &format!(r#"{{"lot":{lot_id},"buyer":"{who}"}}"#));
    }

    /// Anyone, after the review: release the money to the seller, or refund.
    pub fn finalize(&mut self, lot_id: u64) {
        let mut e = self.escrow(lot_id);
        require!(e.phase == Phase::Review, "not in review");
        require!(env::block_timestamp() >= e.review_ends_ns, "review still running");
        let lot = self.lot(lot_id);
        let failed = e.objections * 10_000 >= e.raised * OBJECT_BPS;
        let (token, amount) = e.delivered.clone().expect("delivered in review");
        if failed {
            e.phase = Phase::Refunding;
            self.escrows.insert(lot_id, e);
            self.pay(&Some(token), &lot.seller, amount); // the seller takes the delivery back
        } else {
            e.phase = Phase::Released;
            let raised = e.raised;
            self.escrows.insert(lot_id, e);
            self.pay(&lot.pay, &lot.seller, raised);
        }
        self.emit("finalize", &format!(r#"{{"lot":{lot_id},"accepted":{}}}"#, !failed));
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
    /// of the delivered tokens. Refunding: what they paid.
    pub fn claim(&mut self, lot_id: u64, account: AccountId) {
        let e = self.escrow(lot_id);
        let key = stake_key(lot_id, &account);
        let mut s = self.stakes.get(&key).cloned().unwrap_or_else(|| env::panic_str("no stake"));
        require!(!s.claimed, "already claimed");
        s.claimed = true;
        self.stakes.insert(key, s.clone());
        match e.phase {
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
            review_ends_ns: e.review_ends_ns, objections: U128(e.objections), deliver_by_ns: p.deliver_by_ns, pledged: p.amount,
        })
    }

    pub fn get_stake(&self, lot_id: u64, account_id: AccountId) -> Option<StakeView> {
        let s = self.stakes.get(&stake_key(lot_id, &account_id))?;
        Some(StakeView { paid: U128(s.paid), shares: U128(s.shares), objected: s.objected, claimed: s.claimed })
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
        let earliest = match &lot.sale { Sale::Auction(a) => a.ends_ns, _ => now } + DAY_NS;
        require!(pledge.deliver_by_ns >= earliest, "deadline: at least a day after the sale can end");
        require!(pledge.deliver_by_ns <= now + 365 * DAY_NS, "deadline: within a year");
        lot.pledge = Some(pledge);
        self.lots.insert(lot_id, lot);
        self.escrows.insert(lot_id, Escrow { raised: 0, sold: 0, delivered: None, review_ends_ns: 0, objections: 0, phase: Phase::Funding });
    }

    /// Sales and bids on an idea are only allowed while it is funding.
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
        if let Sale::Auction(a) = &lot.sale {
            require!(a.settled, "settle the auction first");
        }
        // Pledge pro rata to the shares actually sold, rounded up.
        let need = math::wide::mul_div_up(pledge.amount.0, e.sold, crate::units_of(&lot.item));
        require!(amount >= need, "less than pledged");
        if let Sale::Listing { open, .. } = &mut lot.sale {
            *open = false; // no more sales once delivered
        }
        self.lots.insert(lot_id, lot);
        e.delivered = Some((token.clone(), need));
        e.phase = Phase::Review;
        e.review_ends_ns = env::block_timestamp() + REVIEW_NS;
        self.escrows.insert(lot_id, e);
        self.emit("deliver", &format!(r#"{{"lot":{lot_id},"token":"{token}","amount":"{need}"}}"#));
        amount - need
    }
}
