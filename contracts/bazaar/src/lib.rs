//! BAZAAR: sell anything on NEAR at your price, or in a falling-price auction.
//!
//! A LOT is one thing for sale:
//!   - tokens (any NEP-141: memecoins, bridged BTC or SOL, stablecoins...),
//!   - an NFT (any NEP-171),
//!   - an IDEA ("pre-IPO"): the seller pledges tokens by a deadline (e.g.
//!     "10% of FROG by 1 Dec"). Buyers' money waits in escrow until the
//!     seller delivers and buyers accept it (ideas.rs).
//! Each lot has its LORE (title, story, image, links) and is sold either at a
//! fixed price (a LISTING) or at a FALLING price (a Dutch auction, rules.rs),
//! partial buys allowed for tokens and idea shares. Payment is native NEAR or
//! any token the seller chooses.
//!
//! Money safety:
//!   - Items are held by this contract until sold; the seller can cancel any
//!     time and gets back what is left.
//!   - The item reaches the buyer BEFORE the seller is paid.
//!   - Sellers pay a 5% fee on what they receive (kept as owed to the treasury).
//!   - Failed payouts are kept as "owed" and can be withdrawn later.

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

use rules::{Falling, Price, RuleError};

pub const HOUR_NS: u64 = 60 * 60 * 1_000_000_000;
/// The seller's fee on every payment they receive: 5%.
pub const FEE_BPS: u128 = 500;

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
    /// `price.ask` for `price.amount`; `left` still for sale. With `falling`,
    /// the ask falls over time (a Dutch auction) and `price.ask` is the start.
    Listing { price: Price, left: u128, open: bool, falling: Option<Falling> },
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
    /// A falling price: the total ask goes from `start` down to `floor` over
    /// `hours` (1 to 720, default 24), then stays at the floor. The floor is
    /// the seller's reserve: nothing ever sells below it.
    Falling {
        start: U128,
        floor: U128,
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
    /// Who has bought from a lot ("lot|account"): they, and the seller, may
    /// enter the lot's private chat. Each pays for their own entry.
    participants: LookupMap<String, ()>,
    /// Where sellers' fees go.
    treasury: AccountId,
    /// How long backers have to accept or reject an idea's delivery.
    review_ns: u64,
}

#[near]
impl Bazaar {
    /// `review_ns`: how long backers review an idea's delivery (default 72h,
    /// at least a minute; tests use short ones).
    #[init]
    pub fn new(treasury: AccountId, review_ns: Option<u64>) -> Self {
        let review_ns = review_ns.unwrap_or(ideas::REVIEW_NS);
        require!(review_ns >= 60_000_000_000, "review: at least a minute");
        Bazaar {
            treasury,
            review_ns,
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
    /// `pledge.deliver_by_ns`, split into `shares` (default 1: sold whole).
    /// Buyers' money is held until delivery is accepted (ideas.rs).
    pub fn sell_idea(&mut self, terms: SaleTerms, shares: Option<U128>, pledge: ideas::Pledge, pay: Option<AccountId>, lore: Lore) -> u64 {
        let seller = env::predecessor_account_id();
        let before = env::storage_usage();
        let shares = shares.map_or(1, |s| s.0);
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

    /// Seller only. A sale: whatever is left comes back. An idea: any time
    /// before delivery, and every backer gets a refund. 1 yoctoNEAR proves a
    /// full-access key signed.
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
        let Sale::Listing { price, left, open, falling } = &mut lot.sale else { env::panic_str("not for sale yet") };
        require!(*open, "listing closed");
        // A falling price: what the whole lot costs right now.
        let now_price = Price { amount: price.amount, ask: falling.map_or(price.ask, |f| f.ask_at(env::block_timestamp())) };
        let f = rules::fill(now_price, *left, paid, whole).unwrap_or_else(|e| fail(e));
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

    /// Pay a seller `amount`, minus the 5% fee (kept as owed to the treasury).
    pub(crate) fn pay_seller(&mut self, asset: &Option<AccountId>, seller: &AccountId, amount: u128) {
        let fee = amount * FEE_BPS / 10_000;
        if fee > 0 {
            let t = self.treasury.clone();
            self.add_owed(asset, &t, fee);
        }
        self.pay(asset, seller, amount - fee);
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
            Sale::Listing { price: Price { amount: units, ask: ask.0 }, left: units, open: true, falling: None }
        }
        SaleTerms::Falling { start, floor, hours } => {
            let hours = hours.unwrap_or(24) as u64;
            let f = Falling::new(start.0, floor.0, now, hours * HOUR_NS).unwrap_or_else(|e| fail(e));
            Sale::Listing { price: Price { amount: units, ask: start.0 }, left: units, open: true, falling: Some(f) }
        }
    }
}

pub(crate) fn sale_name(s: &Sale) -> &'static str {
    match s {
        Sale::Pending(_) => "pending",
        Sale::Listing { falling: None, .. } => "listing",
        Sale::Listing { falling: Some(_), .. } => "falling",
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
