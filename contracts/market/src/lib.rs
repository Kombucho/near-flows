//! The market contract: a thin NEAR shell around `engine::Book`.
//!
//! The engine does all the market logic (and is tested on its own). This
//! contract only does what a blockchain needs:
//!   - receive tokens  (receiver.rs: NEP-141 `ft_on_transfer`)
//!   - send tokens     (payout.rs: `ft_transfer` + a callback if it fails)
//!   - charge storage  (storage.rs: users prepay for the bytes they add)
//!   - user actions    (user.rs: claim, withdraw, stake, vote, crank)
//!   - read-only views (views.rs)
//!
//! Rust notes for TypeScript developers:
//!   - `u128` is a plain number inside the contract; `U128` is the same number
//!     as a JSON string, because JavaScript cannot hold 128-bit integers.
//!   - `#[near(contract_state)]` marks the struct saved on chain between calls.
//!   - `#[init]` is the constructor, called once at deploy time.

mod delegated;
mod payout;
mod receiver;
mod storage;
mod user;
mod views;

#[cfg(test)]
mod tests;

use engine::{Book, Config};
use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, BorshStorageKey, PanicOnDefault};

const DAY_NS: u64 = 86_400 * 1_000_000_000;

/// Dynamic fee, chosen from the simulator (3,600 runs per policy): a low base
/// fee in calm markets, +2 bps per (bin of recent movement)^2, capped at 5%.
/// With a 30 bps base it kept take-profit fills intact (1.48x in pump-dump)
/// while paying ~60% more in wild markets than a fixed 1% fee.
const DYN_K: u16 = 2;
const MAX_FEE_BPS: u16 = 500;

/// Protocol slice: at most 10% of each fee, and only while the coin is busy
/// (see engine: off below the volume threshold, phasing in up to 2x it).
const PROTOCOL_BPS: u16 = 1_000;

fn with_fees(mut cfg: Config, protocol_threshold: u128) -> Config {
    cfg.dyn_k = DYN_K;
    cfg.max_fee_bps = MAX_FEE_BPS.max(cfg.fee_bps);
    cfg.protocol_bps = PROTOCOL_BPS;
    cfg.protocol_threshold = protocol_threshold;
    cfg
}

/// Prefixes for the contract's storage maps (each map needs a unique one).
#[near]
#[derive(BorshStorageKey)]
enum Key {
    Storage,
    Owed,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Market {
    /// Can only pause and unpause trading, and launch the market once.
    owner: AccountId,
    token: AccountId, // the coin
    quote: AccountId, // what it trades against (wNEAR: wrap.testnet)
    native: bool,
    fee_bps: u16,
    /// Trailing daily quote volume above which the protocol slice phases in.
    protocol_threshold: u128,
    /// None until launched. Everything market-related lives in here: a few KB
    /// loaded on every call, plus bins, positions and stakers in their own
    /// storage slots, loaded only when a call touches them (engine store.rs).
    book: Option<Book>,
    paused: bool,
    /// Tokens the owner sent before launch (they become the opening liquidity).
    funded_token: u128,
    funded_quote: u128,
    /// Prepaid storage per account, in yoctoNEAR (see storage.rs).
    storage: LookupMap<AccountId, u128>,
    /// Payouts that failed (e.g. the receiver was not registered on the token),
    /// kept here so the user can pull them later. Key: "token|account".
    owed: LookupMap<String, u128>,
}

#[near]
impl Market {
    #[init]
    /// `protocol_fee_after_daily_volume`: in the quote token's smallest units.
    /// Below it the protocol takes nothing; it is fixed forever at deploy.
    pub fn new(
        owner: AccountId, token: AccountId, quote: AccountId, native: bool, fee_bps: u16,
        protocol_fee_after_daily_volume: near_sdk::json_types::U128,
    ) -> Self {
        require!(fee_bps > 0 && fee_bps <= MAX_FEE_BPS, "base fee must be between 0.01% and 5%");
        require!(protocol_fee_after_daily_volume.0 > 0, "threshold must be positive");
        Market {
            owner, token, quote, native, fee_bps, book: None, paused: false,
            protocol_threshold: protocol_fee_after_daily_volume.0,
            funded_token: 0, funded_quote: 0,
            storage: LookupMap::new(Key::Storage),
            owed: LookupMap::new(Key::Owed),
        }
    }

    /// Native coin: open the market from the funded auction proceeds.
    /// Opening price = funded quote / funded tokens (the auction price).
    pub fn launch_native(&mut self, bins_per_side: i32) {
        self.assert_can_launch();
        require!(self.native, "this market is for an adopted coin");
        let cfg = with_fees(Config::standard(1, self.fee_bps), self.protocol_threshold); // price set by graduate()
        let now = env::block_timestamp();
        self.book = Some(Book::graduate(cfg, self.funded_quote, self.funded_token, bins_per_side, now));
        self.emit("launch", &format!(r#"{{"kind":"native","bins_per_side":{bins_per_side}}}"#));
    }

    /// Adopted coin: open at `base_price` (quote per token, 1e18 fixed-point),
    /// with the owner's funded tokens as their own liquidity, locked `lock_days`.
    pub fn launch_adopted(&mut self, base_price: near_sdk::json_types::U128, bins_per_side: i32, lock_days: u32) {
        self.assert_can_launch();
        require!(!self.native, "this market is for a native coin");
        let now = env::block_timestamp();
        let cfg = with_fees(Config::adopted(base_price.0, self.fee_bps), self.protocol_threshold);
        let lock = now + lock_days as u64 * DAY_NS;
        let owner = self.owner.to_string();
        let (book, _) = Book::seed(cfg, &owner, self.funded_token, self.funded_quote, bins_per_side, lock, now)
            .unwrap_or_else(|e| env::panic_str(&format!("seed failed: {e:?}")));
        self.book = Some(book);
        self.emit("launch", r#"{"kind":"adopted"}"#);
    }

    /// Owner only. Blocks buys, sells, new liquidity and the crank.
    /// Never blocks claims, withdrawals or unstaking: users can always leave.
    pub fn pause(&mut self) {
        self.assert_owner();
        self.paused = true;
        self.emit("paused", "{}");
    }

    pub fn unpause(&mut self) {
        self.assert_owner();
        self.paused = false;
        self.emit("unpaused", "{}");
    }

    /// Owner only: collect the protocol slice accrued so far. This is the
    /// owner's ONLY way to take money out, and it can only reach this slice.
    #[payable]
    pub fn withdraw_protocol_fees(&mut self) {
        near_sdk::assert_one_yocto();
        self.assert_owner();
        let (q, t) = self.book().take_protocol_fees();
        let owner = self.owner.clone();
        self.pay_both(&owner, q, t);
        self.emit("protocol_fees", &format!(r#"{{"quote":"{q}","token":"{t}"}}"#));
    }
}

/// Helpers that are not callable from outside (no `#[near]` on this impl).
impl Market {
    fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
    }

    fn assert_can_launch(&self) {
        self.assert_owner();
        require!(self.book.is_none(), "already launched");
        require!(self.funded_token > 0 && self.funded_quote > 0, "fund both tokens first");
    }

    fn book(&mut self) -> &mut Book {
        self.book.as_mut().unwrap_or_else(|| env::panic_str("market not launched"))
    }

    fn book_ref(&self) -> &Book {
        self.book.as_ref().unwrap_or_else(|| env::panic_str("market not launched"))
    }

    /// NEP-297 event: a log line wallets and indexers can parse.
    fn emit(&self, event: &str, data_json: &str) {
        env::log_str(&format!(
            r#"EVENT_JSON:{{"standard":"liquidity-layer","version":"1.0.0","event":"{event}","data":[{data_json}]}}"#
        ));
    }
}
