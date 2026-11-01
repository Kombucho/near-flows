//! Unit tests for the contract shell. They run in a simulated NEAR
//! environment (no chain): we set who is calling, with what deposit, and when.
//! Promises (outgoing transfers) are recorded but not executed here; the
//! sandbox integration tests cover real token transfers.

use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::test_utils::VMContextBuilder;
use near_sdk::{testing_env, AccountId, NearToken, PromiseOrValue};

use crate::Market;

const NEAR: u128 = 1_000_000_000_000_000_000_000_000; // 10^24 yocto
const ONE: u128 = 1_000_000_000_000_000_000; // 1 coin / 1 wNEAR in these tests (18 decimals)
const DAY_NS: u64 = 86_400 * 1_000_000_000;

fn acc(s: &str) -> AccountId {
    s.parse().unwrap()
}

/// Set the caller, attached deposit and block time for the next call.
fn as_caller(who: &str, deposit_yocto: u128, now_ns: u64) {
    let mut ctx = VMContextBuilder::new();
    ctx.current_account_id(acc("market.testnet"))
        .predecessor_account_id(acc(who))
        .signer_account_id(acc(who))
        .attached_deposit(NearToken::from_yoctonear(deposit_yocto))
        .account_balance(NearToken::from_near(100))
        .block_timestamp(now_ns);
    testing_env!(ctx.build());
}

/// `who` sends `amount` of `token` to the market with `msg`. Returns the refund.
fn send(m: &mut Market, token: &str, who: &str, amount: u128, msg: &str) -> u128 {
    as_caller(token, 1, DAY_NS);
    match m.ft_on_transfer(acc(who), U128(amount), msg.to_string()) {
        PromiseOrValue::Value(v) => v.0,
        PromiseOrValue::Promise(_) => panic!("expected a value"),
    }
}

fn deposit_storage(m: &mut Market, who: &str) {
    as_caller(who, NEAR, 0);
    m.storage_deposit(None);
}

/// A launched native market: 4_000 wNEAR raised, 500_000_000 coins to the pool.
fn native_market() -> Market {
    as_caller("owner.testnet", 0, 0);
    let mut m = Market::new(acc("owner.testnet"), acc("coin.testnet"), acc("wrap.testnet"), true, 30);
    deposit_storage(&mut m, "owner.testnet");
    send(&mut m, "coin.testnet", "owner.testnet", 500_000_000 * ONE, r#"{"action":"fund"}"#);
    send(&mut m, "wrap.testnet", "owner.testnet", 4_000 * ONE, r#"{"action":"fund"}"#);
    as_caller("owner.testnet", 0, 0);
    m.launch_native(230);
    save(&m);
    deposit_storage(&mut m, "alice.testnet");
    m
}

/// On chain, NEAR saves the contract state after every call. Calling methods
/// directly in a unit test skips that, so we save by hand after setup steps.
fn save(m: &Market) {
    near_sdk::env::state_write(m);
}

#[test]
fn launch_sets_the_auction_price() {
    let m = native_market();
    let c = m.get_config();
    assert!(c.launched && c.native);
    // 4_000 / 500_000_000 = 0.000008 wNEAR per coin, as 1e18 fixed-point
    assert_eq!(c.base_price.0, 8_000_000_000_000);
}

#[test]
fn buy_with_an_exit_plan() {
    let mut m = native_market();
    let refund = send(&mut m, "wrap.testnet", "alice.testnet", 10 * ONE, r#"{"action":"buy","min_out":"0","ladder":true}"#);
    assert_eq!(refund, 0);
    let mine = m.get_positions(acc("alice.testnet"));
    // Inside the 24h launch window the ladder is forced and locked: 4 rungs.
    assert_eq!(mine.len(), 4);
    assert!(mine.iter().all(|p| p.take_profit));
    assert!(m.get_config().price.0 > 8_000_000_000_000, "the buy moved the price up");
}

#[test]
fn ladder_coins_you_already_hold() {
    let mut m = native_market();
    let refund = send(&mut m, "coin.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"ladder"}"#);
    assert_eq!(refund, 0, "all of it goes on the 4 rungs, 25% each");
    assert_eq!(m.get_positions(acc("alice.testnet")).len(), 4);
}

#[test]
#[should_panic(expected = "unknown token")]
fn fake_token_is_rejected() {
    let mut m = native_market();
    send(&mut m, "evil.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"buy","min_out":"0"}"#);
}

#[test]
#[should_panic(expected = "not enough storage balance")]
fn no_storage_deposit_no_positions() {
    let mut m = native_market();
    send(&mut m, "coin.testnet", "bob.testnet", 1_000 * ONE, r#"{"action":"ladder"}"#);
}

#[test]
#[should_panic(expected = "Slippage")]
fn slippage_protection_aborts_the_buy() {
    let mut m = native_market();
    send(&mut m, "wrap.testnet", "alice.testnet", 10 * ONE, r#"{"action":"buy","min_out":"999999999999999999999999999"}"#);
}

#[test]
fn stake_and_vote() {
    let mut m = native_market();
    send(&mut m, "coin.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"stake"}"#);
    as_caller("alice.testnet", 0, DAY_NS);
    m.set_vote([0, 10_000, 0]);
    let s = m.get_staker(acc("alice.testnet")).unwrap();
    assert_eq!((s.staked.0, s.vote), (1_000 * ONE, [0, 10_000, 0]));
    assert_eq!(m.get_stats().votes_burn.0, 1_000 * ONE * 10_000);
}

#[test]
#[should_panic(expected = "paused")]
fn pause_blocks_buys() {
    let mut m = native_market();
    as_caller("owner.testnet", 0, DAY_NS);
    m.pause();
    send(&mut m, "wrap.testnet", "alice.testnet", 10 * ONE, r#"{"action":"buy","min_out":"0"}"#);
}

#[test]
fn pause_never_blocks_leaving() {
    let mut m = native_market();
    send(&mut m, "coin.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"ladder"}"#);
    let id = m.get_positions(acc("alice.testnet"))[0].id;
    as_caller("owner.testnet", 0, DAY_NS);
    m.pause();
    as_caller("alice.testnet", 1, 2 * DAY_NS);
    m.withdraw(id); // must not panic
    assert_eq!(m.get_positions(acc("alice.testnet")).len(), 3);
}

#[test]
#[should_panic(expected = "owner only")]
fn only_owner_can_pause() {
    let mut m = native_market();
    as_caller("alice.testnet", 0, DAY_NS);
    m.pause();
}

#[test]
#[should_panic(expected = "NotNative")]
fn adopted_coins_cannot_be_staked() {
    as_caller("owner.testnet", 0, 0);
    let mut m = Market::new(acc("owner.testnet"), acc("coin.testnet"), acc("wrap.testnet"), false, 30);
    deposit_storage(&mut m, "owner.testnet");
    send(&mut m, "coin.testnet", "owner.testnet", 1_000_000 * ONE, r#"{"action":"fund"}"#);
    send(&mut m, "wrap.testnet", "owner.testnet", 100 * ONE, r#"{"action":"fund"}"#);
    as_caller("owner.testnet", 0, 0);
    m.launch_adopted(U128(100_000_000_000_000), 230, 30); // price 0.0001
    save(&m);
    deposit_storage(&mut m, "alice.testnet");
    send(&mut m, "coin.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"stake"}"#);
}

#[test]
#[should_panic(expected = "Requires attached deposit of exactly 1 yoctoNEAR")]
fn claims_need_one_yocto() {
    let mut m = native_market();
    as_caller("alice.testnet", 0, DAY_NS);
    m.claim(0);
}

#[test]
fn crank_runs_and_is_rate_limited() {
    let mut m = native_market();
    send(&mut m, "wrap.testnet", "alice.testnet", 10 * ONE, r#"{"action":"buy","min_out":"0"}"#);
    as_caller("keeper.testnet", 0, DAY_NS);
    m.crank();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        as_caller("keeper.testnet", 0, DAY_NS + 1);
        m.crank();
    }));
    assert!(result.is_err(), "a second crank in the same hour must fail");
}

#[test]
fn obligations_view_matches_what_came_in() {
    let mut m = native_market();
    send(&mut m, "wrap.testnet", "alice.testnet", 10 * ONE, r#"{"action":"buy","min_out":"0","ladder":true}"#);
    let (q, t) = m.get_obligations();
    // In: 4_000 + 10 wNEAR. Out: none yet in quote (the kept coins left the book).
    assert!(q.0 <= 4_010 * ONE, "owes {} wNEAR units", q.0);
    assert!(t.0 <= 500_000_000 * ONE);
}

#[test]
fn users_pay_for_their_own_storage() {
    let mut m = native_market();
    let before = m.storage_balance_of(acc("alice.testnet")).0;
    send(&mut m, "coin.testnet", "alice.testnet", 1_000 * ONE, r#"{"action":"ladder"}"#);
    let paid = before - m.storage_balance_of(acc("alice.testnet")).0;
    // 4 positions: some hundreds of bytes at 10^19 yocto/byte, well under 0.05 NEAR.
    assert!(paid > 0, "creating positions must cost storage");
    assert!(paid < NEAR / 20, "paid {paid} yocto");
    // Closing one gives part of it back.
    let id = m.get_positions(acc("alice.testnet"))[0].id;
    as_caller("alice.testnet", 1, 2 * DAY_NS);
    m.withdraw(id);
    assert!(m.storage_balance_of(acc("alice.testnet")).0 > before - paid, "freed bytes were credited");
}

#[test]
fn fee_is_dynamic_after_launch() {
    let mut m = native_market();
    assert_eq!(m.get_config().current_fee_bps, 30, "calm: base fee");
    send(&mut m, "wrap.testnet", "alice.testnet", 400 * ONE, r#"{"action":"buy","min_out":"0"}"#);
    assert!(m.get_config().current_fee_bps > 30, "after a big move the fee is higher");
}
