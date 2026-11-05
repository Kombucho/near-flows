//! Contract unit tests (simulated chain). Token and NFT transfers are only
//! scheduled here; the sandbox tests check real ones.

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_contract_standards::non_fungible_token::core::NonFungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{testing_env, AccountId, NearToken};

use crate::ideas::{Phase, Pledge, REVIEW_NS};
use crate::FEE_BPS;
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
    let mut b = Bazaar::new(acc("treasury.near"), None);
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
fn an_nft_falling_price_sale_in_near() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "carol");
    ctx("alice", 0, 0);
    let item = SaleItem::Nft { contract: acc("art.near"), token_id: "1".into() };
    b.open_sale(item, SaleTerms::Falling { start: U128(1_000), floor: U128(400), hours: Some(6) }, None, lore("Genesis #1"));
    ctx("art.near", 0, 0);
    let keep = b.nft_on_transfer(acc("alice"), acc("alice"), "1".into(), r#"{"action":"fund","lot":0}"#.into());
    assert!(matches!(keep, near_sdk::PromiseOrValue::Value(false)), "the NFT stays here, for sale");
    let lot = b.get_lot(0).unwrap();
    assert_eq!((lot.sale.as_str(), lot.ask, lot.floor), ("falling", Some(U128(1_000)), Some(U128(400))));
    // Three hours in, the price is halfway down: 700. Bob pays 650: not enough, refused.
    ctx("bob", 650, 3 * HOUR_NS);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| b.buy(0)));
    assert!(r.is_err(), "below the current price");
    ctx("carol", 900, 3 * HOUR_NS);
    b.buy(0);
    let ev = events().join("\n");
    assert!(ev.contains(r#""paid":"700""#), "Carol paid the price at that moment, 200 refunded: {ev}");
    assert!(!b.get_lot(0).unwrap().open, "sold");
}

#[test]
fn the_price_holds_at_the_floor_and_nobody_can_push_it_up() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "shill");
    ctx("alice", 0, 0);
    b.open_sale(SaleItem::Ft { token: acc("coin.near") }, SaleTerms::Falling { start: U128(1_000), floor: U128(100), hours: Some(10) }, None, lore("Coins"));
    ctx("coin.near", 0, 0);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000), r#"{"action":"fund","lot":0}"#.into());
    // A "shill" buying a little at the top doesn't change the price anyone else pays.
    ctx("shill", 100, 0);
    b.buy(0); // 100 units at 1 per unit
    ctx("bob", 0, 5 * HOUR_NS);
    assert_eq!(b.get_lot(0).unwrap().ask, Some(U128(550)), "time alone sets the price");
    ctx("bob", 0, 99 * HOUR_NS);
    assert_eq!(b.get_lot(0).unwrap().ask, Some(U128(100)), "then it stays at the floor");
    ctx("bob", 90, 99 * HOUR_NS);
    b.buy(0);
    assert_eq!(b.get_lot(0).unwrap().left, Some(U128(0)), "900 units left at 0.1 each: 90 buys them all");
}

#[test]
#[should_panic(expected = "FloorAboveStart")]
fn the_floor_cannot_be_above_the_start() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Falling { start: U128(10), floor: U128(20), hours: None }, None, pledge(), None, lore("x"));
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
#[should_panic(expected = "you cannot buy your own lot")]
fn no_buying_your_own_lot() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(10) }, None, pledge(), None, lore("x"));
    ctx("alice", 50, HOUR_NS);
    b.buy(0);
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn listing_needs_prepaid_storage() {
    ctx("owner", 0, 0);
    let mut b = Bazaar::new(acc("treasury.near"), None);
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
fn an_idea_holds_the_money_until_delivery_and_each_backer_decides_for_themselves() {
    let mut b = funded_idea();
    let e = b.get_escrow(0).unwrap();
    assert_eq!((e.phase, e.raised, e.sold), (Phase::Funding, U128(1_000), U128(100)));
    assert!(!events().iter().any(|e| e.contains("payout")), "the seller was not paid");
    // Alice delivers 1M FROG (the full pledge: all shares sold); 5 extra come back.
    ctx("frog.near", 0, 2 * HOUR_NS);
    let r = b.ft_on_transfer(acc("alice"), U128(1_000_005), r#"{"action":"deliver","lot":0}"#.into());
    assert!(matches!(r, near_sdk::PromiseOrValue::Value(U128(5))));
    assert_eq!(phase(&b), Phase::Review);
    // Carol (40 shares, paid 400) rejects. Bob (60 shares) does nothing: he accepts.
    ctx("carol", 0, 3 * HOUR_NS);
    b.reject(0);
    ctx("dave", 0, 2 * HOUR_NS + REVIEW_NS);
    b.finalize(0);
    assert_eq!(phase(&b), Phase::Released);
    let ev = events().join("\n");
    assert!(ev.contains(r#""paid_to_seller":"600""#) && ev.contains(r#""rejected_shares":"40""#), "{ev}");
    // The fee: 5% of the 600 Alice receives is owed to the treasury.
    assert_eq!(b.owed_of(None, acc("treasury.near")), U128(600 * FEE_BPS / 10_000));
    b.claim(0, acc("bob")); // 60% of the FROG
    b.claim(0, acc("carol")); // her 400 back
    let ev = events().join("\n");
    assert!(ev.contains(r#""buyer":"carol""#), "{ev}");
}

#[test]
fn fake_backers_cannot_overrule_real_ones() {
    // The old 50% vote could be won by the seller's own accounts. Now there is
    // nothing to win: Alice's sock puppet accepting changes nothing for Bob.
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    deposit(&mut b, "sock");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(1_000) }, Some(U128(100)), pledge(), None, lore("FROG"));
    ctx("sock", 900, HOUR_NS); // the seller's puppet buys 90%
    b.buy(0);
    ctx("bob", 100, HOUR_NS);
    b.buy(0);
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("bob", 0, 3 * HOUR_NS);
    b.reject(0); // a fake delivery: Bob wants his money back
    ctx("dave", 0, 2 * HOUR_NS + REVIEW_NS);
    b.finalize(0);
    assert!(b.get_stake(0, acc("bob")).unwrap().rejected);
    b.claim(0, acc("bob"));
    let ev = events().join("\n");
    assert!(ev.contains(r#""paid_to_seller":"900""#), "Alice only gets her own puppet's money (minus the fee): {ev}");
}

#[test]
#[should_panic(expected = "not in review")]
fn rejecting_after_the_review_is_too_late() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("carol", 0, 2 * HOUR_NS + REVIEW_NS);
    b.reject(0);
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
fn only_backers_can_reject() {
    let mut b = funded_idea();
    ctx("frog.near", 0, 2 * HOUR_NS);
    let _ = b.ft_on_transfer(acc("alice"), U128(1_000_000), r#"{"action":"deliver","lot":0}"#.into());
    ctx("mallory", 0, 3 * HOUR_NS);
    b.reject(0);
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
fn the_private_room_is_the_seller_and_everyone_who_bought() {
    let mut b = bazaar("alice");
    deposit(&mut b, "bob");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(100) }, Some(U128(10)), pledge(), None, lore("x"));
    ctx("bob", 10, HOUR_NS);
    b.buy(0);
    assert!(b.is_participant(0, acc("alice")));
    assert!(b.is_participant(0, acc("bob")));
    assert!(!b.is_participant(0, acc("mallory")));
    assert!(!b.is_participant(9, acc("alice")), "no such lot");
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn a_buyer_pays_for_their_own_entry() {
    let mut b = bazaar("alice");
    ctx("alice", 0, 0);
    b.sell_idea(SaleTerms::Listing { ask: U128(100) }, Some(U128(10)), pledge(), None, lore("x"));
    ctx("mallory", 10, HOUR_NS);
    b.buy(0);
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
#[should_panic(expected = "not for sale yet")]
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
