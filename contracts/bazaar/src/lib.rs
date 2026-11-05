//! BAZAAR: sell anything on NEAR at your price, or auction it.
//!
//! A LOT is one thing for sale:
//!   - tokens (any NEP-141: memecoins, bridged BTC or SOL, stablecoins...),
//!   - an NFT (any NEP-171),
//!   - an IDEA: nothing is held by the contract; the seller promises it
//!     off-chain (e.g. "fund this coin"). Buyers trust the seller.
//! Each lot has its LORE (title, story, image, links) and is sold either at a
//! fixed price (a LISTING, partial buys allowed for tokens) or in an AUCTION
//! (rules.rs). Payment is native NEAR or any token the seller chooses.
//!
//! Money safety:
//!   - Items and bids are held by this contract until the sale ends; the
//!     seller can cancel a listing any time, an auction only before a bid.
//!   - `settle` can be called by anyone after an auction ends: it can only
//!     pay the seller, the winner and the outbid, never the caller.
//!   - Failed token payouts are kept as "owed" and can be withdrawn later.

mod payout;
mod receiver;
pub mod rules;
mod storage;
mod views;

#[cfg(test)]
mod tests;

use near_sdk::json_types::U128;
use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, NearToken, PanicOnDefault};

use rules::{Auction, Close, Price, RuleError};

pub const HOUR_NS: u64 = 60 * 60 * 1_000_000_000;

#[near]
#[derive(BorshStorageKey)]
enum Key {
    Lots,
    Storage,
    Owed,
    OwedNft,
}

/// What is for sale.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Ft { token: AccountId, amount: U128 },
    Nft { contract: AccountId, token_id: String },
    Idea,
}

/// The story told about a lot. Stored on chain, so nobody can change it after listing.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Lore {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub links: Vec<String>,
}

impl Lore {
    fn check(&self) {
        require!(!self.title.trim().is_empty() && self.title.len() <= 100, "title: 1 to 100 characters");
        require!(self.description.len() <= 2_000, "description: at most 2000 characters");
        require!(self.image.as_ref().map_or(true, |i| i.len() <= 300), "image: at most 300 characters");
        require!(self.links.len() <= 5 && self.links.iter().all(|l| l.len() <= 200), "links: at most 5, 200 characters each");
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub enum Sale {
    /// Fixed price: `price.ask` for `price.amount`; `left` still for sale.
    Listing { price: Price, left: u128, open: bool },
    Auction(Auction),
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Lot {
    pub seller: AccountId,
    pub item: Item,
    /// What the seller is paid in: None = native NEAR, Some(token) = that NEP-141.
    pub pay: Option<AccountId>,
    pub lore: Lore,
    pub created_ns: u64,
    pub sale: Sale,
}

impl Lot {
    /// Units for sale: token amount, or 1 for an NFT or an idea.
    fn units(&self) -> u128 {
        match &self.item {
            Item::Ft { amount, .. } => amount.0,
            _ => 1,
        }
    }
    fn whole_only(&self) -> bool {
        !matches!(self.item, Item::Ft { .. })
    }
}

/// How a lot is sold, as given by the seller when creating it.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
#[serde(tag = "sale", rename_all = "snake_case")]
pub enum SaleTerms {
    /// `ask`: total price for everything listed.
    Listing { ask: U128 },
    /// `hours`: 1 to 720 (default 48, like Auction Anything).
    Auction {
        opening: U128,
        #[serde(default)]
        reserve: Option<U128>,
        #[serde(default)]
        hours: Option<u32>,
    },
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Bazaar {
    lots: LookupMap<u64, Lot>,
    /// Lot ids run 0..next_id; the app pages through them.
    next_id: u64,
    storage: LookupMap<AccountId, u128>,
    /// Failed token payouts, key "token|account" ("near|account" for NEAR).
    owed: LookupMap<String, u128>,
    /// NFTs whose delivery failed, key "contract|token_id", value: who may claim it.
    owed_nft: LookupMap<String, AccountId>,
}

#[near]
impl Bazaar {
    #[init]
    pub fn new() -> Self {
        Bazaar {
            lots: LookupMap::new(Key::Lots),
            next_id: 0,
            storage: LookupMap::new(Key::Storage),
            owed: LookupMap::new(Key::Owed),
            owed_nft: LookupMap::new(Key::OwedNft),
        }
    }

    /// Sell an IDEA (nothing held by the contract): a fixed price, or an auction.
    /// The seller promises delivery off-chain; buyers trust the seller.
    pub fn sell_idea(&mut self, terms: SaleTerms, pay: Option<AccountId>, lore: Lore) -> u64 {
        let seller = env::predecessor_account_id();
        let before = env::storage_usage();
        let id = self.create(seller.clone(), Item::Idea, terms, pay, lore);
        self.charge_storage(&seller, before);
        id
    }

    /// Buy a listing that is priced in native NEAR. Attach the payment;
    /// anything not used is refunded.
    #[payable]
    pub fn buy(&mut self, lot_id: u64) {
        let who = env::predecessor_account_id();
        let paid = env::attached_deposit().as_yoctonear();
        require!(self.lot(lot_id).pay.is_none(), "this lot is paid in a token: use ft_transfer_call");
        let refund = self.on_buy(lot_id, &who, paid);
        self.pay(&None, &who, refund);
    }

    /// Bid on an auction priced in native NEAR. Attach your MAXIMUM; you pay
    /// at most that, usually less. If you are outbid it comes back at once.
    #[payable]
    pub fn bid(&mut self, lot_id: u64) {
        let who = env::predecessor_account_id();
        let amount = env::attached_deposit().as_yoctonear();
        require!(self.lot(lot_id).pay.is_none(), "this lot is paid in a token: use ft_transfer_call");
        self.on_bid(lot_id, &who, amount);
    }

    /// Close an auction after its end. Anyone may call it: the item goes to
    /// the winner, the price to the seller, the change to the winner. Unsold:
    /// the item goes back to the seller and any bid back to its bidder.
    pub fn settle(&mut self, lot_id: u64) {
        let mut lot = self.lot(lot_id);
        let Sale::Auction(a) = &mut lot.sale else { env::panic_str("not an auction") };
        let close = a.close(env::block_timestamp()).unwrap_or_else(|e| fail(e));
        self.lots.insert(lot_id, lot.clone());
        match close {
            Close::Sold { winner, price, change } => {
                let winner: AccountId = winner.parse().unwrap();
                self.pay(&lot.pay, &lot.seller, price);
                self.pay(&lot.pay, &winner, change);
                self.give(&lot.item, &winner);
                self.emit("sold", &format!(r#"{{"lot":{lot_id},"winner":"{winner}","price":"{price}"}}"#));
            }
            Close::Unsold { refund } => {
                if let Some((who, amount)) = refund {
                    self.pay(&lot.pay, &who.parse().unwrap(), amount);
                }
                self.give(&lot.item, &lot.seller);
                self.emit("unsold", &format!(r#"{{"lot":{lot_id}}}"#));
            }
        }
    }

    /// Seller only. A listing: whatever is left comes back. An auction: only
    /// before the first bid. 1 yoctoNEAR proves a full-access key signed.
    #[payable]
    pub fn cancel(&mut self, lot_id: u64) {
        near_sdk::assert_one_yocto();
        let who = env::predecessor_account_id();
        let mut lot = self.lot(lot_id);
        require!(lot.seller == who, "seller only");
        let back = match &mut lot.sale {
            Sale::Listing { left, open, .. } => {
                require!(*open, "already closed");
                *open = false;
                std::mem::take(left)
            }
            Sale::Auction(a) => {
                a.cancel().unwrap_or_else(|e| fail(e));
                lot.units()
            }
        };
        self.lots.insert(lot_id, lot.clone());
        if back > 0 {
            self.give(&part_of(&lot.item, back), &who);
        }
        self.emit("cancel", &format!(r#"{{"lot":{lot_id}}}"#));
    }
}

impl Bazaar {
    pub(crate) fn lot(&self, id: u64) -> Lot {
        self.lots.get(&id).cloned().unwrap_or_else(|| env::panic_str("no such lot"))
    }

    /// Create a lot (its item is already held by the contract, or is an idea).
    pub(crate) fn create(&mut self, seller: AccountId, item: Item, terms: SaleTerms, pay: Option<AccountId>, lore: Lore) -> u64 {
        lore.check();
        let now = env::block_timestamp();
        let units = match &item { Item::Ft { amount, .. } => amount.0, _ => 1 };
        require!(units > 0, "nothing to sell");
        let sale = match terms {
            SaleTerms::Listing { ask } => {
                require!(ask.0 > 0, "price must be above 0");
                Sale::Listing { price: Price { amount: units, ask: ask.0 }, left: units, open: true }
            }
            SaleTerms::Auction { opening, reserve, hours } => {
                let hours = hours.unwrap_or(48) as u64;
                let a = Auction::new(opening.0, reserve.map_or(0, |r| r.0), now, hours * HOUR_NS).unwrap_or_else(|e| fail(e));
                Sale::Auction(a)
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let kind = if matches!(sale, Sale::Listing { .. }) { "listing" } else { "auction" };
        self.lots.insert(id, Lot { seller: seller.clone(), item, pay, lore, created_ns: now, sale });
        self.emit("lot", &format!(r#"{{"lot":{id},"seller":"{seller}","sale":"{kind}"}}"#));
        id
    }

    /// A buy of `paid`. Pays the seller and hands over the item; returns the refund.
    pub(crate) fn on_buy(&mut self, lot_id: u64, who: &AccountId, paid: u128) -> u128 {
        let mut lot = self.lot(lot_id);
        let whole = lot.whole_only();
        let Sale::Listing { price, left, open } = &mut lot.sale else { env::panic_str("not a listing: bid instead") };
        require!(*open, "listing closed");
        let f = rules::fill(*price, *left, paid, whole).unwrap_or_else(|e| fail(e));
        *left -= f.out;
        if *left == 0 {
            *open = false;
        }
        self.lots.insert(lot_id, lot.clone());
        self.pay(&lot.pay, &lot.seller, f.used);
        self.give(&part_of(&lot.item, f.out), who);
        self.emit("buy", &format!(r#"{{"lot":{lot_id},"buyer":"{who}","units":"{}","paid":"{}"}}"#, f.out, f.used));
        paid - f.used
    }

    /// A bid of `amount` (the bidder's maximum, now held here).
    pub(crate) fn on_bid(&mut self, lot_id: u64, who: &AccountId, amount: u128) {
        let mut lot = self.lot(lot_id);
        require!(&lot.seller != who, "you cannot bid on your own lot");
        let Sale::Auction(a) = &mut lot.sale else { env::panic_str("not an auction: buy instead") };
        let r = a.bid(who.as_str(), amount, env::block_timestamp()).unwrap_or_else(|e| fail(e));
        let ends = a.ends_ns;
        self.lots.insert(lot_id, lot.clone());
        if let Some((back_to, back)) = r.refund {
            self.pay(&lot.pay, &back_to.parse().unwrap(), back);
        }
        self.emit("bid", &format!(r#"{{"lot":{lot_id},"bidder":"{who}","leads":{},"price":"{}","ends_ns":{ends}}}"#, r.leads, r.price));
    }

    /// NEP-297 event, for the app, indexers and the chat server.
    pub(crate) fn emit(&self, event: &str, data_json: &str) {
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"bazaar","version":"1.0.0","event":"{event}","data":[{data_json}]}}"#));
    }
}

/// The same item, `units` of it (tokens), or the item itself (NFT, idea).
fn part_of(item: &Item, units: u128) -> Item {
    match item {
        Item::Ft { token, .. } => Item::Ft { token: token.clone(), amount: U128(units) },
        other => other.clone(),
    }
}

/// A rule broken aborts the call, which on NEAR undoes everything in it
/// (and a token or NFT sent with the call goes back to its sender).
pub(crate) fn fail(e: RuleError) -> ! {
    env::panic_str(&format!("{e:?}"))
}

#[allow(dead_code)]
const _ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
