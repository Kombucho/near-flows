//! BAZAAR: sell anything on NEAR at your price, or auction it.
//!
//! A LOT is one thing for sale:
//!   - tokens (any NEP-141: memecoins, bridged BTC or SOL, stablecoins...),
//!   - an NFT (any NEP-171),
//!   - an IDEA ("pre-IPO"): the seller pledges tokens by a deadline (e.g.
//!     "10% of FROG by 1 Dec"). Buyers' money waits in escrow until the
//!     seller delivers and buyers accept it (ideas.rs).
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

mod ideas;
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
    Escrows,
    Stakes,
    Participants,
}

/// What is for sale.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Item {
    Ft { token: AccountId, amount: U128 },
    Nft { contract: AccountId, token_id: String },
    /// A pledge split into `shares` (1 for an auction: the winner takes all).
    Idea { shares: U128 },
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
        require!(self.image.as_ref().map_or(true, |i| i.len() <= 300 && safe_url(i)), "image: an https:// URL, at most 300 characters");
        require!(self.links.len() <= 5 && self.links.iter().all(|l| l.len() <= 200 && safe_url(l)), "links: at most 5 https:// URLs, 200 characters each");
    }
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub enum Sale {
    /// Opened by the seller, waiting for the item (`fund`). Nothing to buy yet.
    Pending(SaleTerms),
    /// Fixed price: `price.ask` for `price.amount`; `left` still for sale.
    Listing { price: Price, left: u128, open: bool },
    Auction(Auction),
    /// The seller withdrew it before funding.
    Withdrawn,
}

/// The item of a token or NFT sale, named by the seller in `open_sale`.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SaleItem {
    Ft { token: AccountId },
    Nft { contract: AccountId, token_id: String },
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
    /// Ideas only: what the seller promises to deliver, and by when.
    pub pledge: Option<ideas::Pledge>,
}

impl Lot {
    /// Units for sale: token amount, idea shares, or 1 for an NFT.
    fn units(&self) -> u128 {
        units_of(&self.item)
    }
    fn whole_only(&self) -> bool {
        self.units() == 1
    }
}

/// How a lot is sold, as given by the seller when creating it.
#[near(serializers = [borsh, json])]
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
    /// Ideas: the money raised per lot, and each buyer's stake ("lot|account").
    escrows: LookupMap<u64, ideas::Escrow>,
    stakes: LookupMap<String, ideas::Stake>,
    /// Who has bought from or bid on a lot ("lot|account"): they, and the
    /// seller, may enter the lot's private chat. Each pays for their own entry.
    participants: LookupMap<String, ()>,
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
            escrows: LookupMap::new(Key::Escrows),
            stakes: LookupMap::new(Key::Stakes),
            participants: LookupMap::new(Key::Participants),
        }
    }

    /// Step 1 of selling tokens or an NFT: the SELLER (this call is signed by
    /// them) names the item and the terms. Step 2: they send the item with
    /// {"action":"fund","lot":ID}. The item must come from the contract named
    /// here and from this seller, so nobody can open sales in someone else's
    /// name with a fake token. The sale starts when funded.
    pub fn open_sale(&mut self, item: SaleItem, terms: SaleTerms, pay: Option<AccountId>, lore: Lore) -> u64 {
        let seller = env::predecessor_account_id();
        let before = env::storage_usage();
        build_sale(&terms, 1, env::block_timestamp()); // check the terms now
        let item = match item {
            SaleItem::Ft { token } => Item::Ft { token, amount: U128(0) },
            SaleItem::Nft { contract, token_id } => Item::Nft { contract, token_id },
        };
        let id = self.insert_lot(seller.clone(), item, Sale::Pending(terms), pay, lore);
        self.charge_storage(&seller, before);
        id
    }

    /// Sell an IDEA: pledge `pledge.amount` tokens (for all `shares`) by
    /// `pledge.deliver_by_ns`. A listing sells shares; an auction sells it whole.
    /// Buyers' money is held until delivery is accepted (ideas.rs).
    pub fn sell_idea(&mut self, terms: SaleTerms, shares: Option<U128>, pledge: ideas::Pledge, pay: Option<AccountId>, lore: Lore) -> u64 {
        let seller = env::predecessor_account_id();
        let before = env::storage_usage();
        let shares = match terms { SaleTerms::Auction { .. } => 1, SaleTerms::Listing { .. } => shares.map_or(1, |s| s.0) };
        let id = self.insert_lot(seller.clone(), Item::Idea { shares: U128(shares) }, build_sale(&terms, shares, env::block_timestamp()), pay, lore);
        self.attach_pledge(id, pledge);
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
                self.pay(&lot.pay, &winner, change);
                if matches!(lot.item, Item::Idea { .. }) {
                    self.escrow_add(lot_id, &winner, price, 1); // held until delivery is accepted
                } else {
                    self.sell_item(lot_id, &lot.item, &winner, price); // seller paid on arrival
                }
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
    /// before the first bid. An idea: any time before delivery, and every
    /// backer gets a refund. 1 yoctoNEAR proves a full-access key signed.
    #[payable]
    pub fn cancel(&mut self, lot_id: u64) {
        near_sdk::assert_one_yocto();
        let who = env::predecessor_account_id();
        let mut lot = self.lot(lot_id);
        require!(lot.seller == who, "seller only");
        if matches!(lot.item, Item::Idea { .. }) && self.escrow(lot_id).sold > 0 {
            self.abandon(lot_id); // closes the sale; backers claim refunds
            self.emit("cancel", &format!(r#"{{"lot":{lot_id}}}"#));
            return;
        }
        let back = match &mut lot.sale {
            Sale::Pending(_) => {
                lot.sale = Sale::Withdrawn;
                0
            }
            Sale::Withdrawn => env::panic_str("already withdrawn"),
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

    pub(crate) fn insert_lot(&mut self, seller: AccountId, item: Item, sale: Sale, pay: Option<AccountId>, lore: Lore) -> u64 {
        lore.check();
        let id = self.next_id;
        self.next_id += 1;
        let kind = sale_name(&sale);
        self.lots.insert(id, Lot { seller: seller.clone(), item, pay, lore, created_ns: env::block_timestamp(), sale, pledge: None });
        self.emit("lot", &format!(r#"{{"lot":{id},"seller":"{seller}","sale":"{kind}"}}"#));
        id
    }

    /// Step 2: the item arrives. `from` = who the token or NFT contract says
    /// sent it; `contract` = that contract (the predecessor, which can't lie
    /// about itself). Returns nothing: any mismatch aborts and refunds.
    pub(crate) fn fund(&mut self, lot_id: u64, from: &AccountId, contract: &AccountId, amount: u128, token_id: Option<&str>) {
        let mut lot = self.lot(lot_id);
        require!(&lot.seller == from, "only the seller funds a sale");
        let Sale::Pending(terms) = lot.sale.clone() else { env::panic_str("not waiting for funds") };
        match (&mut lot.item, token_id) {
            (Item::Ft { token, amount: a }, None) => {
                require!(token == contract, "wrong token for this sale");
                require!(amount > 0, "nothing sent");
                *a = U128(amount);
            }
            (Item::Nft { contract: c, token_id: t }, Some(id)) => {
                require!(c == contract && t == id, "wrong NFT for this sale");
            }
            _ => env::panic_str("wrong kind of item for this sale"),
        }
        let before = env::storage_usage();
        lot.sale = build_sale(&terms, units_of(&lot.item), env::block_timestamp());
        self.lots.insert(lot_id, lot.clone());
        self.charge_storage(from, before);
        self.emit("funded", &format!(r#"{{"lot":{lot_id},"sale":"{}"}}"#, sale_name(&lot.sale)));
    }

    /// A buy of `paid`. Pays the seller and hands over the item; returns the refund.
    pub(crate) fn on_buy(&mut self, lot_id: u64, who: &AccountId, paid: u128) -> u128 {
        let mut lot = self.lot(lot_id);
        require!(&lot.seller != who, "you cannot buy your own lot");
        let whole = lot.whole_only();
        let idea = matches!(lot.item, Item::Idea { .. });
        if idea {
            self.assert_funding(lot_id);
        }
        let Sale::Listing { price, left, open } = &mut lot.sale else { env::panic_str("not a listing: bid instead") };
        require!(*open, "listing closed");
        let f = rules::fill(*price, *left, paid, whole).unwrap_or_else(|e| fail(e));
        *left -= f.out;
        if *left == 0 {
            *open = false;
        }
        self.lots.insert(lot_id, lot.clone());
        // The buyer's entries (participant, stake) are stored on chain: they pay that storage.
        let before = env::storage_usage();
        self.join(lot_id, who);
        if idea {
            self.escrow_add(lot_id, who, f.used, f.out);
        } else {
            // Item first; the seller is paid only once it has really arrived.
            self.sell_item(lot_id, &part_of(&lot.item, f.out), who, f.used);
        }
        self.charge_storage(who, before);
        self.emit("buy", &format!(r#"{{"lot":{lot_id},"buyer":"{who}","units":"{}","paid":"{}"}}"#, f.out, f.used));
        paid - f.used
    }

    /// A bid of `amount` (the bidder's maximum, now held here).
    pub(crate) fn on_bid(&mut self, lot_id: u64, who: &AccountId, amount: u128) {
        let mut lot = self.lot(lot_id);
        require!(&lot.seller != who, "you cannot bid on your own lot");
        if matches!(lot.item, Item::Idea { .. }) {
            self.assert_funding(lot_id);
        }
        let Sale::Auction(a) = &mut lot.sale else { env::panic_str("not an auction: buy instead") };
        let r = a.bid(who.as_str(), amount, env::block_timestamp()).unwrap_or_else(|e| fail(e));
        let ends = a.ends_ns;
        self.lots.insert(lot_id, lot.clone());
        // A bidder's participant entry costs storage: they pay it (so bid spam
        // from fresh accounts cannot run up the contract's storage bill).
        let before = env::storage_usage();
        self.join(lot_id, who);
        self.charge_storage(who, before);
        if let Some((back_to, back)) = r.refund {
            self.pay(&lot.pay, &back_to.parse().unwrap(), back);
        }
        self.emit("bid", &format!(r#"{{"lot":{lot_id},"bidder":"{who}","leads":{},"price":"{}","ends_ns":{ends}}}"#, r.leads, r.price));
    }

    fn join(&mut self, lot_id: u64, who: &AccountId) {
        let key = format!("{lot_id}|{who}");
        if !self.participants.contains_key(&key) {
            self.participants.insert(key, ());
        }
    }

    /// NEP-297 event, for the app, indexers and the chat server.
    pub(crate) fn emit(&self, event: &str, data_json: &str) {
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"bazaar","version":"1.0.0","event":"{event}","data":[{data_json}]}}"#));
    }
}

/// Turn terms into a live sale of `units`, starting `now`.
fn build_sale(terms: &SaleTerms, units: u128, now: u64) -> Sale {
    require!(units > 0, "nothing to sell");
    match terms {
        SaleTerms::Listing { ask } => {
            require!(ask.0 > 0, "price must be above 0");
            Sale::Listing { price: Price { amount: units, ask: ask.0 }, left: units, open: true }
        }
        SaleTerms::Auction { opening, reserve, hours } => {
            let hours = hours.unwrap_or(48) as u64;
            Sale::Auction(Auction::new(opening.0, reserve.map_or(0, |r| r.0), now, hours * HOUR_NS).unwrap_or_else(|e| fail(e)))
        }
    }
}

pub(crate) fn sale_name(s: &Sale) -> &'static str {
    match s {
        Sale::Pending(_) => "pending",
        Sale::Listing { .. } => "listing",
        Sale::Auction(_) => "auction",
        Sale::Withdrawn => "withdrawn",
    }
}

pub(crate) fn units_of(item: &Item) -> u128 {
    match item {
        Item::Ft { amount, .. } => amount.0,
        Item::Idea { shares } => shares.0,
        Item::Nft { .. } => 1,
    }
}

/// The same item, `units` of it (tokens), or the item itself (NFT, idea).
fn part_of(item: &Item, units: u128) -> Item {
    match item {
        Item::Ft { token, .. } => Item::Ft { token: token.clone(), amount: U128(units) },
        Item::Idea { .. } => Item::Idea { shares: U128(units) },
        other => other.clone(),
    }
}

/// Only plain https URLs: no `javascript:` or `data:` links that would run in
/// a visitor's browser, no spaces, quotes or angle brackets to break out of HTML.
fn safe_url(u: &str) -> bool {
    u.starts_with("https://") && u.len() > 8 && !u.chars().any(|c| c.is_whitespace() || c.is_control() || "\"'<>`\\".contains(c))
}

/// A rule broken aborts the call, which on NEAR undoes everything in it
/// (and a token or NFT sent with the call goes back to its sender).
pub(crate) fn fail(e: RuleError) -> ! {
    env::panic_str(&format!("{e:?}"))
}

#[allow(dead_code)]
const _ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
