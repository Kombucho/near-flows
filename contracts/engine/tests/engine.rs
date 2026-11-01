//! Engine tests. Prices start at 1.0, bins are 1% apart, the fee is 1%.
//! The treasury puts 1_000_000 per bin in three bins on each side.

use engine::{Book, Config, Error, Side, DEFAULT_LADDER};
use math::PRICE_SCALE;

const UNIT: u128 = 1_000_000;
const DAY: u64 = 86_400 * 1_000_000_000;

fn cfg() -> Config {
    Config::standard(PRICE_SCALE, 100) // bin 0 = 1.0 quote per token, 1% fee
}

fn book() -> Book {
    // Tokens in bins 1,2,3 and quote in bins -1,-2,-3. Not locked.
    Book::seed(cfg(), "treasury", 3 * UNIT, 3 * UNIT, 3, 0, 0).unwrap().0
}

#[test]
fn small_buy_stays_in_first_bin() {
    let mut b = book();
    // 100_000 quote: fee 1000, net 99_000, at price 1.01 -> 98_019.8 tokens -> 98_019
    let r = b.swap(Side::Buy, 100_000, 0).unwrap();
    assert_eq!((r.out, r.fee, r.refund), (98_019, 1_000, 0));
    assert_eq!(b.active, 1);
    assert_eq!(b.bins[&1].token, UNIT - 98_019);
    assert_eq!(b.bins[&1].quote, 99_000);
}

#[test]
fn big_buy_crosses_into_the_next_bin() {
    let mut b = book();
    let r = b.swap(Side::Buy, 1_500_000, 0).unwrap();
    assert_eq!(b.bins[&1].token, 0); // bin 1 sold out
    assert!(b.bins[&2].token < UNIT); // bin 2 partly filled
    assert_eq!(b.active, 2);
    assert!(r.out > UNIT && r.out < 2 * UNIT);
    assert_eq!(r.used + r.refund, 1_500_000);
}

#[test]
fn slippage_fails_and_changes_nothing() {
    let mut b = book();
    let before = b.bins.clone();
    let err = b.swap(Side::Buy, 100_000, 99_000).unwrap_err();
    assert_eq!(err, Error::Slippage { out: 98_019, min_out: 99_000 });
    assert_eq!(b.bins, before);
    assert_eq!(b.active, 0);
}

#[test]
fn order_bigger_than_all_liquidity_gets_refund() {
    let mut b = book();
    let r = b.swap(Side::Buy, 10 * UNIT, 0).unwrap();
    assert_eq!(r.out, 3 * UNIT); // every token in the book
    assert!(r.refund > 0);
    assert_eq!(r.used + r.refund, 10 * UNIT);
}

#[test]
fn buy_then_sell_back_costs_about_two_fees() {
    let mut b = book();
    let bought = b.swap(Side::Buy, 100_000, 0).unwrap().out;
    let back = b.swap(Side::Sell, bought, 0).unwrap().out;
    assert!(back < 100_000 && back > 97_900, "got {back}");
}

#[test]
fn lp_earns_the_lp_slice_and_cannot_claim_twice() {
    let mut b = book();
    b.swap(Side::Buy, 100_000, 0).unwrap(); // fee 1000 -> LPs 450, pot 450, creator 100
    assert_eq!(b.pots.pot_quote, 450);
    assert_eq!(b.pots.creator_quote, 100);
    let bin1 = *b.positions.iter().find(|(_, p)| p.bin == 1).unwrap().0;
    let (q, _) = b.claim(bin1, "treasury").unwrap();
    assert!(q == 449 || q == 450, "got {q}"); // at most 1 unit of rounding dust
    assert_eq!(b.claim(bin1, "treasury").unwrap(), (0, 0));
}

#[test]
fn only_owner_can_claim() {
    let mut b = book();
    assert_eq!(b.claim(0, "mallory"), Err(Error::NotOwner));
}

#[test]
fn wrong_side_liquidity_is_rejected() {
    let mut b = book();
    assert_eq!(b.add_liquidity("alice", 5, 0, 100, 0, 0), Err(Error::WrongSide));
    assert_eq!(b.add_liquidity("alice", -5, 100, 0, 0, 0), Err(Error::WrongSide));
}

#[test]
fn locked_liquidity_cannot_leave_early() {
    let mut b = book();
    let id = b.add_liquidity("dev", 4, UNIT, 0, 1_000, 0).unwrap();
    assert_eq!(b.withdraw(id, "dev", 999), Err(Error::Locked { until_ns: 1_000 }));
    assert_eq!(b.withdraw(id, "dev", 1_000).unwrap().token, UNIT);
}

#[test]
fn withdraw_after_being_filled_returns_quote() {
    let mut b = book();
    b.swap(Side::Buy, 2_000_000, 0).unwrap(); // empties bin 1, partly bin 2
    let bin1 = *b.positions.iter().find(|(_, p)| p.bin == 1).unwrap().0;
    let w = b.withdraw(bin1, "treasury", 0).unwrap();
    // The LP's tokens were sold at 1.01: they now get 1_010_000 quote back.
    assert_eq!((w.token, w.quote), (0, 1_010_000));
    assert!(w.fee_quote > 0);
}

// ---------- graduation, vault, creator slice, crank, ladder ----------

#[test]
fn graduation_opens_at_the_auction_price() {
    // 4_000 quote raised, 500_000 tokens to the pool -> price 0.008
    let mut b = Book::graduate(cfg(), 4_000 * UNIT, 500_000 * UNIT, 50, 0);
    assert_eq!(b.cfg.base_price, PRICE_SCALE * 8 / 1_000);
    assert_eq!(b.protocol_pos.len(), 100); // 50 bins each side
    assert_eq!(b.withdraw(0, engine::PROTOCOL, u64::MAX - 1), Err(Error::Locked { until_ns: u64::MAX }));
}

#[test]
fn crank_pays_stakers_and_burns_by_vote() {
    let mut b = book();
    b.stake("alice", 300).unwrap();
    b.stake("bob", 100).unwrap();
    b.set_vote("alice", [0, 10_000, 0]).unwrap(); // all burn
    b.set_vote("bob", [10_000, 0, 0]).unwrap(); // all dividend
    b.swap(Side::Buy, 1_000_000, 0).unwrap(); // fee 10_000 -> pot 4_500, creator 1_000
    let r = b.crank(1).unwrap();
    // No locked liquidity, so the creator slice joins the pot: 5_500 (+ protocol LP fees: none).
    // Votes: 75% burn, 25% dividend.
    assert_eq!(r.dividend_q, 5_500 / 4);
    assert!(r.burned > 0);
    let (dq, _) = b.claim_dividends("alice").unwrap();
    assert_eq!(dq, 5_500 / 4 * 300 / 400); // Alice voted burn but still shares the dividend
}

#[test]
fn crank_is_rate_limited() {
    let mut b = book();
    b.crank(DAY).unwrap();
    assert!(matches!(b.crank(DAY + 1), Err(Error::TooSoon { .. })));
    assert!(b.crank(DAY + 3_600 * 1_000_000_000).is_ok());
}

#[test]
fn unstake_waits_seven_days_and_stops_earning() {
    let mut b = book();
    b.stake("alice", 1_000).unwrap();
    b.request_unstake("alice", 1_000, 0).unwrap();
    assert!(matches!(b.withdraw_unstaked("alice", 6 * DAY), Err(Error::Locked { .. })));
    assert_eq!(b.withdraw_unstaked("alice", 7 * DAY).unwrap(), 1_000);
    assert_eq!(b.vault.total_stake, 0);
    assert_eq!(b.vault.votes, Default::default());
}

#[test]
fn creator_slice_goes_to_locked_liquidity_only() {
    let mut b = book();
    // dev locks 40 days (eligible), degen locks 1 day (not eligible).
    let dev = b.add_liquidity("dev", 10, UNIT, 0, 40 * DAY, 0).unwrap();
    let degen = b.add_liquidity("degen", 11, UNIT, 0, DAY, 0).unwrap();
    b.swap(Side::Buy, 1_000_000, 0).unwrap(); // creator slice 1_000
    b.crank(1).unwrap();
    let (dev_q, _) = b.claimable(dev).unwrap();
    let (degen_q, _) = b.claimable(degen).unwrap();
    assert!(dev_q >= 999, "dev got {dev_q}");
    assert_eq!(degen_q, 0);
}

#[test]
fn creator_slice_stops_when_lock_expires() {
    let mut b = book();
    let dev = b.add_liquidity("dev", 10, UNIT, 0, 31 * DAY, 0).unwrap();
    b.crank(32 * DAY).unwrap(); // lock expired before this crank
    b.swap(Side::Buy, 1_000_000, 0).unwrap();
    b.crank(33 * DAY).unwrap();
    assert_eq!(b.claimable(dev).unwrap().0, 0);
    assert_eq!(b.creator.total_weight, 0);
}

#[test]
fn ladder_places_asks_at_multiples() {
    let mut b = book();
    let after_launch = DAY; // outside the 24h launch window: the buyer's own choice applies
    let buy = b.buy("ape", 100_000, 0, &DEFAULT_LADDER, after_launch).unwrap();
    assert!(!buy.locked);
    assert_eq!(buy.ladder.len(), 4);
    assert_eq!(buy.kept, buy.swap.out - 4 * (buy.swap.out * 1_250 / 10_000));
    let bins: Vec<i32> = buy.ladder.iter().map(|i| b.positions[i].bin).collect();
    assert_eq!(bins, vec![1 + 41, 1 + 70, 1 + 111, 1 + 162]); // from active bin 1
    assert!(buy.ladder.iter().all(|i| b.positions[i].locked_until_ns == 0));
}

#[test]
fn plain_spot_buy_after_launch_keeps_everything() {
    let mut b = book();
    let buy = b.buy("ape", 100_000, 0, &[], DAY).unwrap();
    assert_eq!(buy.kept, buy.swap.out);
    assert!(buy.ladder.is_empty());
}

#[test]
fn launch_window_locks_half_of_every_buy_for_30_days() {
    let mut b = book(); // launched at t = 0, window 24h
    let sniper = b.buy("sniper", 100_000, 0, &[], 60 * 1_000_000_000).unwrap(); // 1 minute in
    assert!(sniper.locked);
    // Asked for plain spot, but inside the window 4 x 12.5% goes on the locked ladder.
    assert_eq!(sniper.kept, sniper.swap.out - 4 * (sniper.swap.out * 1_250 / 10_000));
    let id = sniper.ladder[0];
    assert!(b.positions[&id].creator_weight > 0); // locked depth earns the creator slice
    assert!(matches!(b.withdraw(id, "sniper", 10 * DAY), Err(Error::Locked { .. })));
}

/// Simple deterministic random numbers, so the test is repeatable.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

#[test]
fn solvent_after_5000_random_actions_of_every_kind() {
    let mut b = book();
    let mut rng = Lcg(7);
    // Real balances the contract would hold: everything in minus everything out.
    let (mut q_bal, mut t_bal) = (3 * UNIT, 3 * UNIT);
    let mut now = 0u64;
    let users = ["a", "b", "c", "d"];
    for step in 0..5_000u64 {
        now += 600 * 1_000_000_000; // 10 minutes
        let who = users[(rng.next() % 4) as usize];
        match rng.next() % 9 {
            0 | 1 | 2 => if let Ok(r) = b.swap(Side::Buy, rng.next() as u128 % 200_000 + 1, 0) { q_bal += r.used; t_bal -= r.out; },
            3 | 4 => if let Ok(r) = b.swap(Side::Sell, rng.next() as u128 % 200_000 + 1, 0) { t_bal += r.used; q_bal -= r.out; },
            5 => { let a = rng.next() as u128 % 50_000 + 1; if b.stake(who, a).is_ok() { t_bal += a; } }
            6 => { let s = b.vault.stakers.get(who).map(|s| s.stake).unwrap_or(0); if s > 0 { b.request_unstake(who, s / 2 + 1, now).ok(); } if let Ok(a) = b.withdraw_unstaked(who, now) { t_bal -= a; } }
            7 => if let Ok((q, t)) = b.claim_dividends(who) { q_bal -= q; t_bal -= t; },
            _ => { let a = rng.next() as u128 % 50_000 + 1; if let Ok((_, placed)) = b.place_ladder(who, a, &DEFAULT_LADDER, 0, now) { t_bal += placed; } }
        }
        let _ = b.crank(now); // anyone can call it; often too soon, that is fine
        if step % 250 == 0 {
            let (oq, ot) = b.obligations();
            assert!(oq <= q_bal && ot <= t_bal - b.burned, "step {step}: owes ({oq},{ot}) holds ({q_bal},{t_bal})");
        }
    }
    // Burned tokens left the contract for good.
    let (oq, ot) = b.obligations();
    assert!(oq <= q_bal, "quote: owes {oq}, holds {q_bal}");
    assert!(ot <= t_bal - b.burned, "token: owes {ot}, holds {}", t_bal - b.burned);
}
