//! Engine tests. Prices start at 1.0, bins are 1% apart, the fee is 1%.
//! The treasury puts 1_000_000 per bin in three bins on each side.

use engine::{Book, Config, Error, Side};
use math::fees::FeeShares;
use math::PRICE_SCALE;

const UNIT: u128 = 1_000_000;

fn book() -> Book {
    let cfg = Config {
        base_price: PRICE_SCALE, // bin 0 = 1.0 quote per token
        step_bps: 100,
        fee_bps: 100,
        shares: FeeShares { pot_bps: 4_500, creator_bps: 1_000 },
    };
    let mut b = Book::new(cfg);
    // Tokens in bins 1,2,3 and quote in bins -1,-2,-3.
    b.add_uniform("treasury", 3 * UNIT, 3 * UNIT, 3, 0).unwrap();
    b
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
    // Emptying bin 1 costs 1_010_000 net + fee 10_203 (1% of gross) = 1_020_203.
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
    // 100_000 in, ~98_000 back: two 1% fees plus rounding.
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
    assert_eq!(b.add_liquidity("alice", 5, 0, 100, 0), Err(Error::WrongSide)); // quote above price
    assert_eq!(b.add_liquidity("alice", -5, 100, 0, 0), Err(Error::WrongSide)); // token below price
}

#[test]
fn locked_liquidity_cannot_leave_early() {
    let mut b = book();
    let id = b.add_liquidity("dev", 4, UNIT, 0, 1_000).unwrap();
    assert_eq!(b.withdraw(id, "dev", 999), Err(Error::Locked { until_ns: 1_000 }));
    let w = b.withdraw(id, "dev", 1_000).unwrap();
    assert_eq!(w.token, UNIT);
}

#[test]
fn withdraw_after_being_filled_returns_quote() {
    let mut b = book();
    b.swap(Side::Buy, 2_000_000, 0).unwrap(); // empties bin 1, partly bin 2
    let bin1 = *b.positions.iter().find(|(_, p)| p.bin == 1).unwrap().0;
    let w = b.withdraw(bin1, "treasury", 0).unwrap();
    // The LP's tokens were sold at 1.01: they now get ~1_010_000 quote back.
    assert_eq!(w.token, 0);
    assert_eq!(w.quote, 1_010_000);
    assert!(w.fee_quote > 0);
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
fn no_value_from_nothing_over_2000_random_trades() {
    let mut b = book();
    let mut rng = Lcg(42);
    // Everything that ever entered the contract, minus everything that left.
    let (mut quote_in, mut token_in) = (3 * UNIT, 3 * UNIT);
    for _ in 0..2_000 {
        let amount = (rng.next() % 300_000) as u128 + 1;
        let side = if rng.next() % 2 == 0 { Side::Buy } else { Side::Sell };
        if let Ok(r) = b.swap(side, amount, 0) {
            match side {
                Side::Buy => { quote_in += r.used; token_in -= r.out; }
                Side::Sell => { token_in += r.used; quote_in -= r.out; }
            }
        }
    }
    // What the contract owes: bin reserves + pots + every LP's unclaimed fees.
    let ids: Vec<u64> = b.positions.keys().copied().collect();
    let (mut owed_q, mut owed_t) = (0u128, 0u128);
    for id in ids {
        let (q, t) = b.claimable(id).unwrap();
        owed_q += q;
        owed_t += t;
    }
    let held_q: u128 = b.bins.values().map(|x| x.quote).sum::<u128>() + b.pots.pot_quote + b.pots.creator_quote + owed_q;
    let held_t: u128 = b.bins.values().map(|x| x.token).sum::<u128>() + b.pots.pot_token + b.pots.creator_token + owed_t;
    // Obligations can never exceed what came in. The difference is rounding dust.
    assert!(held_q <= quote_in, "quote: owes {held_q}, has {quote_in}");
    assert!(held_t <= token_in, "token: owes {held_t}, has {token_in}");
    assert!(quote_in - held_q < 2_000 && token_in - held_t < 2_000, "dust should be tiny");
}
