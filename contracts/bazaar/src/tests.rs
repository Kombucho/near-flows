//! Contract unit tests (simulated chain). Token and NFT transfers are only
//! scheduled here; the sandbox tests check real ones.

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::non_fungible_token::core::NonFungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};

use crate::ideas::{Phase, Pledge, REVIEW_NS};
use crate::{Bazaar, Lore, SaleItem, SaleTerms, HOUR_NS};

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

/// 1M tokens, delivered within 10 days.
fn pledge() -> Pledge {
    Pledge { amount: U128(1_000_000), deliver_by_ns: 240 * HOUR_NS }
}

fn events() -> Vec<String> {
    get_logs().into_iter().filter(|l| l.starts_with("EVENT_JSON")).collect()
}

#[test]
fn list_tokens_and_buy_part_of_them_in_another_token() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "carol");
    // Alice opens a listing for 500 wNEAR total, then sends 1000 COIN to fund it.
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Listing { ask: U128(500) }, Some(acc("wrap.near")), lore("Half my bag"));
    assert_eq!(b.get_lot(0).unwrap().sale, "pending");
    ctx("coin.near", 0, 0);
    let refund = b.ft_on_transfer(acc("alice"), U128(1_000), r#"{"action":"fund","lot":0}"#.into());
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
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Listing { ask: U128(500) }, Some(acc("wrap.near")), lore("x"));
    ctx("coin.near", 0, 0);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000), r#"{"action":"fund","lot":0}"#.into());
    ctx("fake.near", 0, 0);
    let _ = b.ft_on_transfer(acc("bob"), U128(500), r#"{"action":"buy","lot":0}"#.into());
}

#[test]
fn an_nft_auction_in_near_with_proxy_bids_and_settle() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "carol");
    ctx("alice", 0, 0);
    let item = SaleItem::Nft { contract: acc("art.near"), token_id: "1".into() };
    b.open_sale(item, SaleTerms::Auction { opening: U128(100), reserve: None, hours: Some(48) }, None, lore("Genesis #1"));
    ctx("art.near", 0, 0);
    let keep = b.nft_on_transfer(acc("alice"), acc("alice"), "1".into(), r#"{"action":"fund","lot":0}"#.into());
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
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: Some(48) }, None, pledge(), None, lore("My coin idea"));
    ctx("bob", 1_000, HOUR_NS);
    b.bid(0);
    ctx("bob", 0, 2 * HOUR_NS);
    b.settle(0);
}

#[test]
fn an_idea_listing_is_bought_whole() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    let id = b.sell_idea(SaleTerms::Listing { ask: U128(5_000) }, None, pledge(), None, lore("A coin about frogs"));
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
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, pledge(), None, lore("x"));
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
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, pledge(), None, lore("x"));
    ctx("alice", 50, HOUR_NS);
    b.bid(0);
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn listing_needs_prepaid_storage() {
    ctx("owner", 0, 0);
    let mut b = Bazaar::new();
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, pledge(), None, lore("x"));
}

#[test]
#[should_panic(expected = "title: 1 to 100 characters")]
fn a_lot_needs_a_title() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, pledge(), None, lore(" "));
}

fn deposit(b: &mut Bazaar, who: &str) {
    ctx(who, NearToken::from_near(1).as_yoctonear(), 0);
    b.storage_deposit(None);
}

/// An idea split into 100 shares at 10 NEAR-units each; Bob buys 60, Carol 40.
fn funded_idea() -> Bazaar {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "carol");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(1_000) }, Some(U128(100)), pledge(), None, lore("FROG"));
    ctx("bob", 600, HOUR_NS);
    b.buy(0);
    ctx("carol", 400, HOUR_NS);
    b.buy(0);
    b
}

fn phase(b: &Bazaar) -> Phase {
    b.get_escrow(0).unwrap().phase
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn backing_an_idea_needs_prepaid_storage() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(1_000) }, Some(U128(100)), pledge(), None, lore("FROG"));
    ctx("dave", 100, HOUR_NS);
    b.buy(0);
}

#[test]
fn an_idea_holds_the_money_until_delivery_is_accepted() {
    let mut b = funded_idea();
    let e = b.get_escrow(0).unwrap();
    assert_eq!((e.phase, e.raised, e.sold), (Phase::Funding, U128(1_000), U128(100)));
    assert!(!events().iter().any(|e| e.contains("payout")), "the seller was not paid");
    // Alice delivers 1M FROG (the full pledge: all shares sold); 5 extra come back.
    ctx("frog.near", 0, 2 * HOUR_NS);
    let r = b.ft_on_transfer(acc("alice"), U128(1_000_005), r#"{"action":"deliver","lot":0}"#.into());
    assert!(matches!(r, near_sdk::PromiseOrValue::Value(U128(5))));
    assert_eq!(phase(&b), Phase::Review);
    // Carol (40% of the money) objects: under 50%, so the delivery stands.
    ctx("carol", 0, 3 * HOUR_NS);
    b.object(0);
    ctx("dave", 0, 2 * HOUR_NS + REVIEW_NS);
    b.finalize(0);
    assert_eq!(phase(&b), Phase::Released);
    b.claim(0, acc("bob"));
    b.claim(0, acc("carol"));
    let ev = events().join("\n");
    assert!(ev.contains(r#""accepted":true"#), "{ev}");
}

#[test]
fn half_the_money_objecting_refunds_everyone() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("bob", 0, 3 * HOUR_NS); // Bob paid 60%
    b.object(0);
    ctx("dave", 0, 2 * HOUR_NS + REVIEW_NS);
    b.finalize(0);
    assert_eq!(phase(&b), Phase::Refunding);
    b.claim(0, acc("carol"));
    assert_eq!(b.get_stake(0, acc("carol")).unwrap().claimed, true);
}

#[test]
fn no_delivery_by_the_deadline_refunds_everyone() {
    let mut b = funded_idea();
    ctx("dave", 0, 240 * HOUR_NS);
    b.expire(0);
    assert_eq!(phase(&b), Phase::Refunding);
    b.claim(0, acc("bob"));
}

#[test]
#[should_panic(expected = "less than pledged")]
fn delivering_less_than_pledged_is_refused() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(999_999), r#"{"action":"deliver","lot":0}"#.into());
}

#[test]
fn a_half_sold_idea_owes_half_the_pledge() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(1_000) }, Some(U128(100)), pledge(), None, lore("FROG"));
    ctx("bob", 500, HOUR_NS);
    b.buy(0);
    ctx("frog.near", 0, 2 * HOUR_NS);
    let r = b.ft_on_transfer(acc("alice"), U128(500_000), r#"{"action":"deliver","lot":0}"#.into());
    assert!(matches!(r, near_sdk::PromiseOrValue::Value(U128(0))));
    assert_eq!(b.get_escrow(0).unwrap().delivered_amount, Some(U128(500_000)));
}

#[test]
#[should_panic(expected = "only the seller delivers")]
fn only_the_seller_can_deliver() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("mallory"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
}

#[test]
#[should_panic(expected = "you hold no stake")]
fn only_backers_can_object() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("mallory", 0, 3 * HOUR_NS);
    b.object(0);
}

#[test]
#[should_panic(expected = "this idea is no longer funding")]
fn no_buying_after_delivery() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("bob", 100, 3 * HOUR_NS);
    b.buy(0);
}

#[test]
#[should_panic(expected = "already claimed")]
fn a_claim_pays_once() {
    let mut b = funded_idea();
    ctx("dave", 0, 240 * HOUR_NS);
    b.expire(0);
    b.claim(0, acc("bob"));
    b.claim(0, acc("bob"));
}

#[test]
fn the_seller_cancelling_refunds_backers() {
    let mut b = funded_idea();
    ctx("alice", 1, 2 * HOUR_NS);
    b.cancel(0);
    assert_eq!(phase(&b), Phase::Refunding);
}

#[test]
fn the_private_room_is_the_seller_and_everyone_who_bid() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, pledge(), None, lore("x"));
    ctx("bob", 50, HOUR_NS);
    b.bid(0);
    assert!(b.is_participant(0, acc("alice")));
    assert!(b.is_participant(0, acc("bob")));
    assert!(!b.is_participant(0, acc("mallory")));
    assert!(!b.is_participant(9, acc("alice")), "no such lot");
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn a_bidder_pays_for_their_own_entry() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Auction { opening: U128(10), reserve: None, hours: None }, None, pledge(), None, lore("x"));
    ctx("mallory", 50, HOUR_NS);
    b.bid(0);
}

// ---- red team: attacks that must fail ----

#[test]
#[should_panic(expected = "only the seller funds a sale")]
fn a_fake_token_cannot_fund_a_sale_in_someone_elses_name() {
    // Mallory's token contract claims "alice sent this" to fund Alice's pending sale.
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Listing { ask: U128(5) }, None, lore("x"));
    ctx("coin.near", 0, 0);
    let _ = b.ft_on_transfer(acc("mallory"), U128(1_000), r#"{"action":"fund","lot":0}"#.into());
}

#[test]
#[should_panic(expected = "wrong token for this sale")]
fn funding_with_a_different_token_is_refused() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("usdc.near") }, SaleTerms::Listing { ask: U128(5) }, None, lore("Real USDC"));
    ctx("fake-usdc.near", 0, 0); // a look-alike token, sent by Alice herself
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000), r#"{"action":"fund","lot":0}"#.into());
}

#[test]
#[should_panic(expected = "bad msg")]
fn a_token_contract_cannot_open_a_sale_by_itself() {
    // The old one-step "sell" message is gone: only a signed open_sale creates lots.
    let mut b = bazaar("alice");
    ctx("evil-token.near", 0, 0);
    let _ = b.ft_on_transfer(acc("alice"), U128(1), r#"{"action":"sell","sale":"listing","ask":"1","lore":{"title":"x"}}"#.into());
}

#[test]
#[should_panic(expected = "wrong NFT for this sale")]
fn funding_with_a_different_nft_is_refused() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Nft { contract: acc("art.near"), token_id: "1".into() }, SaleTerms::Listing { ask: U128(5) }, None, lore("x"));
    ctx("art.near", 0, 0);
    let _ = b.nft_on_transfer(acc("alice"), acc("alice"), "2".into(), r#"{"action":"fund","lot":0}"#.into());
}

#[test]
#[should_panic(expected = "not a listing")]
fn nothing_can_be_bought_before_the_item_arrives() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Listing { ask: U128(5) }, None, lore("x"));
    ctx("bob", 5, HOUR_NS);
    b.buy(0);
}

#[test]
fn lore_links_must_be_plain_https() {
    for bad in ["javascript:alert(1)", "http://x.com", "https://x.com/\"onmouseover=1", "https://a b", "data:text/html,hi", "https://"] {
        let mut b = bazaar("alice");
        ctx("alice", 0, 0);
        let mut l = lore("x");
        l.links = vec![bad.into()];
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, pledge(), None, l.clone());
        }));
        assert!(r.is_err(), "accepted {bad}");
    }
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    let mut l = lore("x");
    l.links = vec!["https://x.com/frog".into()];
    l.image = Some("https://img.example/frog.png".into());
    b.sell_idea(SaleTerms::Listing { ask: U128(5) }, None, pledge(), None, l);
}

#[test]
fn a_seller_can_withdraw_a_sale_before_funding_it() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Listing { ask: U128(5) }, None, lore("x"));
    ctx("alice", 1, 0);
    b.cancel(0);
    assert_eq!(b.get_lot(0).unwrap().sale, "withdrawn");
}
