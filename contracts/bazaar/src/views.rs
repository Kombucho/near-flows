//! Read-only views for the app. Amounts are U128 strings (JavaScript-safe).

use near_sdk::json_types::U128;
use near_sdk::{near, AccountId};

use crate::{Bazaar, BazaarExt, Item, Lore, Sale};

#[near(serializers = [json])]
pub struct LotView {
    pub id: u64,
    pub seller: AccountId,
    pub item: Item,
    /// null = native NEAR
    pub pay: Option<AccountId>,
    pub lore: Lore,
    pub created_ns: u64,
    /// "pending" (waiting for the item), "listing", "auction" or "withdrawn"
    pub sale: String,
    /// open for buys or bids right now (an auction past its end awaits `settle`)
    pub open: bool,
    // Listing
    pub ask: Option<U128>,
    pub left: Option<U128>,
    // Auction
    pub opening: Option<U128>,
    pub reserve: Option<U128>,
    pub price: Option<U128>,
    pub min_bid: Option<U128>,
    pub leader: Option<AccountId>,
    pub bids: Option<u32>,
    pub ends_ns: Option<u64>,
    pub settled: Option<bool>,
}

#[near]
impl Bazaar {
    pub fn get_lot(&self, id: u64) -> Option<LotView> {
        let lot = self.lots.get(&id)?;
        let now = near_sdk::env::block_timestamp();
        let mut v = LotView {
            id, seller: lot.seller.clone(), item: lot.item.clone(), pay: lot.pay.clone(), lore: lot.lore.clone(),
            created_ns: lot.created_ns, sale: String::new(), open: false,
            ask: None, left: None, opening: None, reserve: None, price: None, min_bid: None,
            leader: None, bids: None, ends_ns: None, settled: None,
        };
        v.sale = crate::sale_name(&lot.sale).into();
        match &lot.sale {
            Sale::Pending(_) | Sale::Withdrawn => {}
            Sale::Listing { price, left, open } => {
                v.open = *open;
                v.ask = Some(U128(price.ask));
                v.left = Some(U128(*left));
            }
            Sale::Auction(a) => {
                v.open = !a.settled && now < a.ends_ns;
                v.opening = Some(U128(a.opening));
                v.reserve = Some(U128(a.reserve));
                v.price = Some(U128(a.price));
                v.min_bid = Some(U128(a.min_bid()));
                v.leader = a.leader.as_ref().and_then(|l| l.who.parse().ok());
                v.bids = Some(a.bids);
                v.ends_ns = Some(a.ends_ns);
                v.settled = Some(a.settled);
            }
        }
        Some(v)
    }

    /// Lots with ids from_id..from_id+limit (limit at most 100). Page up to `next_id`.
    pub fn get_lots(&self, from_id: u64, limit: u64) -> Vec<LotView> {
        (from_id..from_id.saturating_add(limit.min(100)).min(self.next_id)).filter_map(|id| self.get_lot(id)).collect()
    }

    /// May `account_id` enter this lot's private chat? The seller, and anyone
    /// who has bought from it or bid on it.
    pub fn is_participant(&self, lot_id: u64, account_id: AccountId) -> bool {
        match self.lots.get(&lot_id) {
            Some(l) if l.seller == account_id => true,
            Some(_) => self.participants.contains_key(&format!("{lot_id}|{account_id}")),
            None => false,
        }
    }

    pub fn next_id(&self) -> u64 {
        self.next_id
    }

    /// An auction's leader's locked maximum is readable on chain by anyone;
    /// this view exists so the app can show the leader their own maximum.
    pub fn get_leader_max(&self, id: u64) -> Option<U128> {
        match &self.lots.get(&id)?.sale {
            Sale::Auction(a) => a.leader.as_ref().map(|l| U128(l.max)),
            _ => None,
        }
    }
}

#[allow(dead_code)]
fn _item_is_json(_: Item) {}
