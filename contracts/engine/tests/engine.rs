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

#[test]
fn crank_survives_a_pot_too_small_to_mint_shares() {
    // Found by the simulator: tiny deepen amounts into big bins minted 0 shares
    // and the crank panicked, which on-chain would block it forever.
    let mut b = Book::graduate(cfg(), 4_000 * UNIT * UNIT, 500_000 * UNIT * UNIT, 50, 0);
    b.stake("alice", 1_000).unwrap();
    b.set_vote("alice", [0, 0, 10_000]).unwrap(); // all deepen
    // Price is 0.008. Selling 20_000 tokens pays a 200-token fee: pot 90 + creator 20
    // (no locked LPs, so it joins the pot) = 110 tokens, all to deepen, 11 per bin.
    // 11 tokens x 0.008 = 0.088 quote, which rounds to 0 value -> 0 shares.
    b.swap(Side::Sell, 20_000, 0).unwrap();
    for h in 1..5u64 {
        b.crank(h * 3_600 * 1_000_000_000).unwrap();
    }
}

#[test]
fn buyback_cannot_be_baited_by_a_far_dust_ask() {
    // Attack: when the asks near the price are gone, leave 1 token at bin 300
    // (~20x the price) and wait for the buyback to pay that price for it.
    let mut b = book();
    b.stake("alice", 1_000).unwrap();
    b.set_vote("alice", [0, 10_000, 0]).unwrap(); // all burn
    b.swap(Side::Buy, 10 * UNIT, 0).unwrap(); // eats every ask, fills the pot
    let bait = b.add_liquidity("attacker", 300, 1, 0, 0, 0).unwrap();
    b.crank(1).unwrap();
    assert_eq!(b.bins[&300].token, 1, "the bait ask must not be bought");
    assert_eq!(b.position_assets(bait).unwrap().1, 0, "attacker earned no quote");
    assert!(b.burn_quote > 0, "unspent buyback money waits for real asks");
}

#[test]
fn a_price_pushed_right_before_the_crank_does_not_steer_deepen() {
    // Attack: push the price up 40 bins, then crank, hoping the protocol places
    // its bids at the inflated price so the attacker can sell into them.
    let mut b = Book::graduate(cfg(), 4_000 * UNIT, 4_000 * UNIT, 100, 0); // price 1.0
    b.stake("alice", 1_000).unwrap();
    b.set_vote("alice", [0, 0, 10_000]).unwrap(); // all deepen
    b.crank(1).unwrap();
    let bins_before = b.protocol_pos.len();
    b.swap(Side::Buy, 1_500 * UNIT, 0).unwrap(); // big push
    assert!(b.active > 30, "price pushed to bin {}", b.active);
    b.crank(2 * 3_600 * 1_000_000_000).unwrap();
    assert_eq!(b.protocol_pos.len(), bins_before, "no new protocol bins at the pushed price");
    assert!(b.pots.pot_quote > 1_000_000, "deepen money waits in the pot, got {}", b.pots.pot_quote);
}

// ---------- one-way take-profits ("the locked drawer") ----------

/// Book with LP tokens in bins 1..3 and quote below, plus Alice's take-profit at bin 5.
fn book_with_alice_tp(tokens: u128) -> (Book, u64) {
    let mut b = book();
    let id = b.place_take_profit("alice", 5, tokens, 0, DAY).unwrap();
    (b, id)
}

#[test]
fn a_filled_take_profit_survives_the_dump() {
    let (mut b, alice) = book_with_alice_tp(UNIT);
    // Rally: a buyer eats bins 1..3 and all of Alice's order at bin 5 (price 1.01^5).
    b.swap(Side::Buy, 5 * UNIT, 0).unwrap();
    let (tokens, quote) = b.position_assets(alice).unwrap();
    assert_eq!(tokens, 0, "all of Alice's tokens sold");
    assert!(quote >= 1_051_000, "Alice was paid ~1.051 per token plus maker fees, got {quote}");
    // Dump: sell a lot back down through bin 5 and below.
    b.swap(Side::Sell, 3 * UNIT, 0).unwrap();
    assert_eq!(b.position_assets(alice).unwrap(), (0, quote), "the dump cannot touch the drawer");
    let (q, _) = b.claim(alice, "alice").unwrap();
    assert_eq!(q, quote);
}

#[test]
fn sellers_never_see_take_profit_proceeds() {
    let (mut b, _) = book_with_alice_tp(UNIT);
    b.swap(Side::Buy, 5 * UNIT, 0).unwrap();
    assert_eq!(b.bins[&5].quote, 0, "proceeds are not in the two-way layer");
}

#[test]
fn partial_fill_is_shared_pro_rata() {
    let mut b = book();
    let alice = b.place_take_profit("alice", 5, 3 * UNIT, 0, DAY).unwrap();
    let bob = b.place_take_profit("bob", 5, UNIT, 0, DAY).unwrap();
    // Bins 1..3 cost ~3_091_000 with fees; the remaining ~209_000 buys part of bin 5.
    b.swap(Side::Buy, 3_300_000, 0).unwrap();
    let sold_at_5 = 4 * UNIT - b.bins[&5].tp_token;
    assert!(sold_at_5 > 0);
    let (a_left, a_q) = b.position_assets(alice).unwrap();
    let (b_left, b_q) = b.position_assets(bob).unwrap();
    // Alice owns 3x Bob's share: 3x the tokens left and 3x the proceeds (±1 for rounding).
    assert!(a_left.abs_diff(3 * b_left) <= 3, "{a_left} vs {b_left}");
    assert!(a_q.abs_diff(3 * b_q) <= 3, "{a_q} vs {b_q}");
}

#[test]
fn a_new_order_after_a_sell_out_does_not_share_old_proceeds() {
    let (mut b, alice) = book_with_alice_tp(UNIT);
    b.swap(Side::Buy, 5 * UNIT, 0).unwrap(); // Alice's round sells out
    let alice_quote = b.position_assets(alice).unwrap().1;
    b.swap(Side::Sell, 1_000, 0).unwrap(); // price moves, bin 5 is still at/above it
    let carol = b.place_take_profit("carol", 6, UNIT, 0, DAY).unwrap();
    let dave = b.place_take_profit("dave", 5, UNIT, 0, DAY).unwrap(); // same bin, new round
    assert_eq!(b.position_assets(dave).unwrap(), (UNIT, 0), "Dave gets none of Alice's money");
    assert_eq!(b.position_assets(alice).unwrap().1, alice_quote, "Alice keeps all of it");
    let _ = carol;
}

#[test]
fn cancel_returns_unsold_tokens() {
    let (mut b, alice) = book_with_alice_tp(UNIT);
    let w = b.withdraw(alice, "alice", DAY).unwrap();
    assert_eq!((w.token, w.fee_quote), (UNIT, 0));
    assert_eq!(b.bins[&5].tp_token, 0);
}

#[test]
fn ladder_now_uses_one_way_orders() {
    let mut b = book();
    let buy = b.buy("ape", 100_000, 0, &DEFAULT_LADDER, DAY).unwrap();
    assert!(buy.ladder.iter().all(|i| b.positions[i].take_profit));
}

// ---------- adopted coins ----------

fn adopted_book() -> Book {
    Book::seed(Config::adopted(PRICE_SCALE, 100), "lp", 3 * UNIT, 3 * UNIT, 3, 0, 0).unwrap().0
}

#[test]
fn adopted_coins_cannot_be_staked() {
    let mut b = adopted_book();
    assert_eq!(b.stake("alice", 1_000), Err(Error::NotNative));
}

#[test]
fn adopted_fees_go_90_to_lps_and_10_to_depth() {
    let mut b = adopted_book();
    b.swap(Side::Buy, 100_000, 0).unwrap(); // fee 1_000 -> pot 100, creator 0, LPs 900
    assert_eq!((b.pots.pot_quote, b.pots.creator_quote), (100, 0));
    let before = b.protocol_pos.len();
    b.crank(1).unwrap();
    assert_eq!(b.burned, 0, "adopted coins never burn");
    assert!(b.protocol_pos.len() > before || b.pots.pot_quote > 0, "the pot went to depth");
}

#[test]
fn adopted_locked_liquidity_earns_no_creator_slice() {
    let mut b = adopted_book();
    let id = b.add_liquidity("dev", 10, UNIT, 0, 40 * DAY, 0).unwrap();
    assert_eq!(b.positions[&id].creator_weight, 0);
    assert!(!b.in_launch_window(0), "no launch window for adopted coins");
}

#[test]
fn adopted_coins_still_get_one_way_take_profits() {
    let mut b = adopted_book();
    let id = b.place_take_profit("alice", 5, UNIT, 0, 0).unwrap();
    b.swap(Side::Buy, 5 * UNIT, 0).unwrap();
    b.swap(Side::Sell, 3 * UNIT, 0).unwrap();
    let (tokens, quote) = b.position_assets(id).unwrap();
    assert_eq!(tokens, 0);
    assert!(quote > UNIT, "Alice's rung sold and the dump did not undo it");
}

// ---------- dynamic fee ----------

fn dynamic_book() -> Book {
    let mut cfg = Config::standard(PRICE_SCALE, 30); // base 0.3%
    cfg.dyn_k = 2; // +2 bps per (bin of movement)^2
    cfg.max_fee_bps = 500; // never above 5%
    Book::seed(cfg, "lp", 30 * UNIT, 30 * UNIT, 30, 0, 0).unwrap().0
}

#[test]
fn calm_market_pays_the_base_fee() {
    let b = dynamic_book();
    assert_eq!(b.fee_at(0, 0), 30);
    assert_eq!(b.fee_at(0, 1), 32); // 30 + 2 * 1^2
    assert_eq!(b.fee_at(0, 5), 80); // 30 + 2 * 5^2
}

#[test]
fn fees_rise_after_the_price_moves_and_are_capped() {
    let mut b = dynamic_book();
    b.swap(Side::Buy, 10 * UNIT, 0).unwrap(); // moves ~10 bins
    assert!(b.vol_bins >= 9, "volatility remembered: {}", b.vol_bins);
    let next = b.fee_at(b.active, b.active);
    assert!(next > 150, "the next trade pays more: {next} bps");
    b.vol_bins = 1_000;
    assert_eq!(b.fee_at(0, 0), 500, "never above the cap");
}

#[test]
fn one_big_trade_pays_an_escalating_fee() {
    let mut calm = dynamic_book();
    let small = calm.swap(Side::Buy, UNIT / 10, 0).unwrap();
    let mut wild = dynamic_book();
    let big = wild.swap(Side::Buy, 10 * UNIT, 0).unwrap();
    let small_rate = small.fee * 10_000 / small.used;
    let big_rate = big.fee * 10_000 / big.used;
    assert!(small_rate <= 33, "small trade ~base fee, got {small_rate} bps");
    assert!(big_rate > 3 * small_rate, "big trade pays much more per unit: {big_rate} bps");
}

#[test]
fn volatility_memory_halves_every_ten_minutes() {
    let mut b = dynamic_book();
    let min = 60 * 1_000_000_000u64;
    b.vol_bins = 40;
    assert_eq!(b.vol_at(10 * min), 20, "one half-life");
    assert_eq!(b.vol_at(20 * min), 10, "two half-lives");
    assert_eq!(b.vol_at(5 * min), 30, "half a half-life (linear in between)");
    assert_eq!(b.vol_at(6 * 60 * min), 0, "gone after hours");
    b.touch(10 * min);
    assert_eq!((b.vol_bins, b.vol_updated_ns), (20, 10 * min));
    b.touch(5 * min); // time never goes backwards
    assert_eq!(b.vol_bins, 20);
}

#[test]
fn the_fee_is_back_near_base_half_an_hour_after_a_pump() {
    let mut b = dynamic_book();
    b.swap(Side::Buy, 10 * UNIT, 0).unwrap(); // a pump: the fee jumps
    assert!(b.current_fee_bps(0) > 150);
    let half_hour = 30 * 60 * 1_000_000_000u64;
    assert!(b.current_fee_bps(half_hour) <= 33, "calm again: {}", b.current_fee_bps(half_hour));
}

#[test]
fn solvent_with_dynamic_fees_over_3000_random_trades() {
    let mut b = dynamic_book();
    let mut rng = Lcg(99);
    let (mut q_bal, mut t_bal) = (30 * UNIT, 30 * UNIT);
    let mut now = 0u64;
    for _ in 0..3_000 {
        now += 300 * 1_000_000_000;
        let amount = rng.next() as u128 % (2 * UNIT) + 1;
        if rng.next() % 2 == 0 {
            if let Ok(r) = b.swap(Side::Buy, amount, 0) { q_bal += r.used; t_bal -= r.out; }
        } else if let Ok(r) = b.swap(Side::Sell, amount, 0) { t_bal += r.used; q_bal -= r.out; }
        let _ = b.crank(now);
    }
    let (oq, ot) = b.obligations();
    assert!(oq <= q_bal && ot <= t_bal - b.burned, "owes ({oq},{ot}) holds ({q_bal},{})", t_bal - b.burned);
}

// ---------- protocol slice (only when busy) ----------

fn book_with_protocol_fee(threshold: u128) -> Book {
    let mut cfg = Config::standard(PRICE_SCALE, 100);
    cfg.protocol_bps = 1_000; // up to 10% of each fee
    cfg.protocol_threshold = threshold;
    Book::seed(cfg, "lp", 30 * UNIT, 30 * UNIT, 30, 0, 0).unwrap().0
}

#[test]
fn protocol_slice_is_off_while_the_coin_is_quiet() {
    let mut b = book_with_protocol_fee(1_000 * UNIT);
    b.swap(Side::Buy, UNIT, 0).unwrap();
    assert_eq!(b.protocol_bps_now(), 0);
    assert_eq!((b.pots.protocol_quote, b.pots.protocol_token), (0, 0));
}

#[test]
fn protocol_slice_phases_in_between_1x_and_2x_the_threshold() {
    let mut b = book_with_protocol_fee(1_000);
    b.daily_volume = 1_000;
    assert_eq!(b.protocol_bps_now(), 0, "at the threshold: still off");
    b.daily_volume = 1_500;
    assert_eq!(b.protocol_bps_now(), 500, "halfway: 5%");
    b.daily_volume = 2_000;
    assert_eq!(b.protocol_bps_now(), 1_000, "at 2x: the full 10%");
    b.daily_volume = 50_000;
    assert_eq!(b.protocol_bps_now(), 1_000, "never more than 10%");
}

#[test]
fn when_busy_the_protocol_takes_10pct_of_the_fee_off_the_top() {
    let mut b = book_with_protocol_fee(1);
    b.daily_volume = 10; // busy
    b.swap(Side::Buy, 100_000, 0).unwrap(); // fee 1_000 -> protocol 100, then 45/10/45 of 900
    assert_eq!(b.pots.protocol_quote, 100);
    assert_eq!(b.pots.pot_quote, 405);
    assert_eq!(b.pots.creator_quote, 90);
    assert_eq!(b.take_protocol_fees(), (100, 0));
    assert_eq!(b.pots.protocol_quote, 0);
}

#[test]
fn daily_volume_tracks_trading_and_fades_when_it_stops() {
    let mut b = book_with_protocol_fee(1);
    b.swap(Side::Buy, 2_400_000, 0).unwrap();
    b.crank(1).unwrap(); // this hour x 24 = 57.6M; the average moves 1/24 of the way
    let busy = b.daily_volume;
    assert!(busy > 2_000_000, "daily volume rose: {busy}");
    for h in 2..50u64 {
        b.crank(h * 3_600 * 1_000_000_000).unwrap(); // two quiet days
    }
    assert!(b.daily_volume < busy / 4, "quiet days bring it down: {}", b.daily_volume);
}

// ---------- merging repeat take-profits ----------

#[test]
fn repeat_orders_at_the_same_rung_merge_into_one_position() {
    let mut b = book();
    let first = b.place_take_profit("alice", 5, UNIT, 0, DAY).unwrap();
    let second = b.place_take_profit("alice", 5, UNIT, 0, DAY).unwrap();
    assert_eq!(first, second, "topped up, not a new position");
    assert_eq!(b.position_assets(first).unwrap(), (2 * UNIT, 0));
    let bob = b.place_take_profit("bob", 5, UNIT, 0, DAY).unwrap();
    assert_ne!(bob, first, "other owners keep their own position");
}

#[test]
fn topping_up_keeps_earlier_proceeds_exactly_once() {
    let mut b = book();
    let id = b.place_take_profit("alice", 5, UNIT, 0, DAY).unwrap();
    b.swap(Side::Buy, 3_300_000, 0).unwrap(); // fills part of Alice's rung
    let (left, earned) = b.position_assets(id).unwrap();
    assert!(earned > 0 && left < UNIT);
    b.place_take_profit("alice", 5, UNIT, 0, DAY).unwrap(); // top up
    let (left2, earned2) = b.position_assets(id).unwrap();
    assert_eq!(earned2, earned, "earlier proceeds kept, not doubled");
    assert!(left2.abs_diff(left + UNIT) <= 1, "{left2} vs {}", left + UNIT);
    assert_eq!(b.claim(id, "alice").unwrap().0, earned);
}

#[test]
fn launch_day_buys_merge_their_rungs() {
    let mut b = book(); // launched at 0, 24h window
    let one = b.buy("ape", 50_000, 0, &[], 3_600 * 1_000_000_000).unwrap();
    let two = b.buy("ape", 50_000, 0, &[], 7_200 * 1_000_000_000).unwrap();
    assert_eq!(one.ladder.len(), 4);
    let ape_positions = b.positions.values().filter(|p| p.owner == "ape").count();
    // Both buys put rungs at multiples of slightly different prices, but the
    // shared ones merge, so the count grows far slower than 4 per buy.
    assert!(ape_positions < 8, "got {ape_positions}");
    let _ = two;
}

// ---------- settle: payouts anyone can trigger, only to the owner ----------

#[test]
fn settle_pays_the_owner_and_closes_a_sold_out_rung() {
    let mut b = book();
    let id = b.place_take_profit("alice", 5, UNIT, 0, DAY).unwrap();
    b.swap(Side::Buy, 5 * UNIT, 0).unwrap(); // Alice's rung sells out
    // (The seeded "treasury" account's LP positions earned fees too, so they are listed as well.)
    assert!(b.settleable(10).contains(&id), "the keeper can find it");
    let s = b.settle(id, DAY).unwrap();
    assert_eq!(s.owner, "alice");
    assert!(s.quote > UNIT && s.closed, "paid ~1.05 per token and closed: {s:?}");
    assert!(!b.positions.contains_key(&id));
    assert!(!b.settleable(10).contains(&id));
}

#[test]
fn settle_keeps_a_partly_filled_rung_open() {
    let mut b = book();
    let id = b.place_take_profit("alice", 5, 4 * UNIT, 0, DAY).unwrap();
    b.swap(Side::Buy, 3_300_000, 0).unwrap(); // only part of it sells
    let s = b.settle(id, DAY).unwrap();
    assert!(s.quote > 0 && !s.closed);
    assert_eq!(b.settle(id, DAY).unwrap().quote, 0, "nothing paid twice");
}

#[test]
fn protocol_positions_cannot_be_settled() {
    let mut b = Book::graduate(cfg(), 4_000 * UNIT, 4_000 * UNIT, 10, 0);
    let pid = *b.protocol_pos.values().next().unwrap();
    assert_eq!(b.settle(pid, 0), Err(Error::NotOwner));
}
