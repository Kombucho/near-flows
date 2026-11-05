//! Contract unit tests (simulated chain). Token and NFT transfers are only
//! scheduled here; the sandbox tests check real ones.

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::non_fungible_token::core::NonFungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};

use crate::{Bazaar, Lore, SaleTerms, HOUR_NS};

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

fn ctx(who: &str, deposit: u128, now: u64) {
    testing_env!(VMContextBuilder::new()
        .current_account_id(acc("bazaar.near"))
        .predecessor_account_id(acc(who))
        .attached_deposit(NearToken::from_yoctonear(deposit))
        .block_timestamp(now)
        .build());
}

fn lore(title: &str) -> Lore {
    Lore { title: title.into(), description: "a story".into(), image: None, links: vec![] }
}

/// A bazaar where `seller` has prepaid storage.
fn bazaar(seller: &str) -> Bazaar {
    ctx("owner", 0, 0);
    let mut b = Bazaar::new();
    ctx(seller, NearToken::from_near(1).as_yoctonear(), 0);
    b.storage_deposit(None);
    b
}

fn events() -> Vec<String> {
    get_logs().into_iter().filter(|l| l.starts_with("EVENT_JSON")).collect()
}

#[test]
fn list_tokens_and_buy_part_of_them_in_another_token() {
    let mut b = bazaar("alice");
    // Alice sends 1000 COIN with a listing for 500 wNEAR total.
    ctx("coin.near", 0, 0);
    let msg = r#"{"action":"sell","sale":"listing","ask":"500","pay":"wrap.near","lore":{"title":"Half my bag"}}"#;
    let refund = b.ft_on_transfer(acc("alice"), U128(1_000), msg.into());
    assert!(matches!(refund, near_sdk::PromiseOrValue::Value(U128(0))));
    let lot = b.get_lot(0).unwrap();
    assert_eq!((lot.sale.as_str(), lot.left, lot.open), ("listing", Some(U128(1_000)), true));
    // Bob pays 100 wNEAR: 200 COIN.
    ctx("wrap.near", 0, HOUR_NS);
    let r = b.ft_on_transfer(acc("bob"), U128(100), r#"{"action":"buy","lot":0}"#.into());
    assert!(matches!(r, near_sdk::PromiseOrValue::Value(U128(0))));
    assert_eq!(b.get_lot(0).unwrap().left, Some(U128(800)));
    assert!(events().iter().any(|e| e.contains(r#""event":"buy""#) && e.contains(r#""units":"200""#)));
    // Carol overpays: takes the rest (800 for 400), 600 refunded.
    let r = b.ft_on_transfer(acc("carol"), U128(1_000), r#"{"action":"buy","lot":0}"#.into());
    assert!(matches!(r, near_sdk::PromiseOrValue::Value(U128(600))));
    assert!(!b.get_lot(0).unwrap().open, "sold out");
}

#[test]
#[should_panic(expected = "this lot is not paid in this token")]
fn paying_with_the_wrong_token_is_refused() {
    let mut b = bazaar("alice");
    ctx("coin.near", 0, 0);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000), r#"{"action":"sell","sale":"listing","ask":"500","pay":"wrap.near","lore":{"title":"x"}}"#.into());
    ctx("fake.near", 0, 0);
    let _ = b.ft_on_transfer(acc("bob"), U128(500), r#"{"action":"buy","lot":0}"#.into());
}

#[test]
fn an_nft_auction_in_near_with_proxy_bids_and_settle() {
    let mut b = bazaar("alice");
    ctx("art.near", 0, 0);
    let msg = r#"{"action":"sell","sale":"auction","opening":"100","hours":48,"lore":{"title":"Genesis #1"}}"#;
    let keep = b.nft_on_transfer(acc("alice"), acc("alice"), "1".into(), msg.into());
    assert!(matches!(keep, near_sdk::PromiseOrValue::Value(false)), "the NFT stays here, for sale");
    ctx("bob", 1_000, HOUR_NS);
    b.bid(0);
    ctx("carol", 300, 2 * HOUR_NS);
    b.bid(0); // carol is outbid at once: bob's maximum is 1000
    let lot = b.get_lot(0).unwrap();
    assert_eq!((lot.leader, lot.price), (Some(acc("bob")), Some(U128(330))));
    ctx("dave", 1, 49 * HOUR_NS);
    b.settle(0);
    let ev = events().join("\n");
    assert!(ev.contains(r#""event":"sold""#) && ev.contains(r#""winner":"bob""#) && ev.contains(r#""price":"330""#), "{ev}");
}

#[test]
#[should_panic(expected = "NotClosed")]
fn nobody_can_settle_early() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: Some(48) }, None, lore("My coin idea"));
    ctx("bob", 1_000, HOUR_NS);
    b.bid(0);
    ctx("bob", 0, 2 * HOUR_NS);
    b.settle(0);
}

#[test]
fn an_idea_listing_is_bought_whole() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    let id = b.sell_idea(SaleTerms::Listing { ask: U128(5_000) }, None, lore("A coin about frogs"));
    ctx("bob", 6_000, HOUR_NS);
    b.buy(id);
    let lot = b.get_lot(id).unwrap();
    assert!(!lot.open);
    assert!(events().iter().any(|e| e.contains(r#""paid":"5000""#)), "paid the price, 1000 refunded");
}

#[test]
#[should_panic(expected = "HasBids")]
fn the_seller_cannot_pull_an_auction_after_a_bid() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, lore("x"));
    ctx("bob", 50, HOUR_NS);
    b.bid(0);
    ctx("alice", 1, 2 * HOUR_NS);
    b.cancel(0);
}

#[test]
#[should_panic(expected = "you cannot bid on your own lot")]
fn no_bidding_on_your_own_lot() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, lore("x"));
    ctx("alice", 50, HOUR_NS);
    b.bid(0);
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn listing_needs_prepaid_storage() {
    ctx("owner", 0, 0);
    let mut b = Bazaar::new();
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, lore("x"));
}

#[test]
#[should_panic(expected = "title: 1 to 100 characters")]
fn a_lot_needs_a_title() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, lore(" "));
}
