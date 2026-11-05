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
    /// "pending" (waiting for the item), "listing", "falling" or "withdrawn"
    pub sale: String,
    /// open for buys right now
    pub open: bool,
    /// The total price of everything listed, right now (it falls over time in a "falling" sale).
    pub ask: Option<U128>,
    pub left: Option<U128>,
    // Falling price
    pub start: Option<U128>,
    pub floor: Option<U128>,
    pub start_ns: Option<u64>,
    pub ends_ns: Option<u64>,
}

#[near]
impl Bazaar {
    pub fn get_lot(&self, id: u64) -> Option<LotView> {
        let lot = self.lots.get(&id)?;
        let now = near_sdk::env::block_timestamp();
        let mut v = LotView {
            id, seller: lot.seller.clone(), item: lot.item.clone(), pay: lot.pay.clone(), lore: lot.lore.clone(),
            created_ns: lot.created_ns, sale: String::new(), open: false,
            ask: None, left: None, start: None, floor: None, start_ns: None, ends_ns: None,
        };
        v.sale = crate::sale_name(&lot.sale).into();
        match &lot.sale {
            Sale::Pending(_) | Sale::Withdrawn => {}
            Sale::Listing { price, left, open, falling } => {
                v.open = *open;
                v.ask = Some(U128(falling.map_or(price.ask, |f| f.ask_at(now))));
                v.left = Some(U128(*left));
                if let Some(f) = falling {
                    v.start = Some(U128(f.start_ask));
                    v.floor = Some(U128(f.floor_ask));
                    v.start_ns = Some(f.start_ns);
                    v.ends_ns = Some(f.end_ns);
                }
            }
        }
        Some(v)
    }

    /// Lots with ids from_id..from_id+limit (limit at most 100). Page up to `next_id`.
    pub fn get_lots(&self, from_id: u64, limit: u64) -> Vec<LotView> {
        (from_id..from_id.saturating_add(limit.min(100)).min(self.next_id)).filter_map(|id| self.get_lot(id)).collect()
    }

    /// May `account_id` enter this lot's private chat? The seller, and anyone
    /// who has bought from it.
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
}

#[allow(dead_code)]
fn _item_is_json(_: Item) {}
