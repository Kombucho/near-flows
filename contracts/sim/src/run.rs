//! One 30-day market, hour by hour.
//!
//! Each hour: the coin's fair value moves (what the wider market thinks it is
//! worth), random traders trade, arbitrageurs pull the book price toward fair
//! value, special events fire, and anyone calls the crank.

use engine::{Book, Config, DEFAULT_LADDER};

use crate::regime::{Event, Regime};
use crate::rng::Rng;
use crate::world::{near, to_f, Faults, Holder, Strategy, World, HOUR, STRATEGIES};

pub const HOURS: u32 = 720;
const RAISED: f64 = 4_000.0; // NEAR raised in the auction (meme.cooking soft cap)
const SUPPLY: f64 = 1e9; // total tokens; half to the pool, half to auction depositors
const ARB_CHUNK: f64 = 20.0; // NEAR per arbitrage trade

#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub fair_mult: f64,
    pub book_mult: f64,
    pub cohort: [f64; 4], // average end value / deposit, per strategy
    pub staker_yield: f64, // dividends received / value staked at launch
    pub burned_pct: f64,
    pub protocol_vs_hold: f64, // protocol liquidity value / just holding its launch assets
    pub volume: f64,
    pub event_pnl: f64, // insider, sniper or sandwich bot profit in NEAR
    /// Buyers who enter with NEAR: end value / NEAR put in, per strategy
    /// [hold, market, market+lock-in, wheel 0.9/0.8, wheel tight, hybrid,
    ///  hybrid+defend, hybrid lock-in spot+defend].
    pub buyers: [f64; 8],
    /// Share of wheel buyers whose bids never filled at all (still all NEAR).
    pub wheel_never_filled: f64,
    pub lag_hours: u32, // hours the book was >25% away from fair value
    pub faults: Faults,
}

struct Ctx { rng: Rng, fair: f64, anchor: f64, event_pnl: f64, stash: u128 }

pub fn run(r: &Regime, seed: u64, fee_bps: u16, bins_per_side: i32, dyn_k: u16, max_fee: u16) -> Outcome {
    let mut cfg = Config::standard(1, fee_bps);
    cfg.dyn_k = dyn_k;
    cfg.max_fee_bps = max_fee.max(fee_bps);
    if !r.launch_lock {
        cfg.launch_window_ns = 0;
    }
    let book = Book::graduate(cfg, near(RAISED), near(SUPPLY / 2.0), bins_per_side, 0);
    let mut w = World { book, now: 0, contract_q: near(RAISED), contract_t: near(SUPPLY / 2.0), faults: Faults::default(), volume_q: 0 };
    let mut c = Ctx { rng: Rng::new(seed), fair: 0.0, anchor: 0.0, event_pnl: 0.0, stash: 0 };
    let insider_share = match r.event { Event::InsiderDump { share, .. } => share, _ => 0.0 };
    let mut holders = make_holders(r, &mut w, &mut c.rng, insider_share);
    let p0 = w.price();
    (c.fair, c.anchor) = (p0, p0);
    let staked0: f64 = holders.iter().map(|h| to_f(w.book.vault.assets(&h.name).0)).sum::<f64>() * p0;
    let mut lag = 0;
    let mut hist: Vec<i32> = Vec::new();
    let mut buyers: Vec<Buyer> = Vec::new();
    for hour in 0..HOURS {
        w.now = hour as u64 * HOUR;
        w.book.touch(w.now); // fee memory decays with time (10-minute half-life)
        move_fair(r, hour, &mut c);
        event(r, hour, &mut w, &mut c, &mut holders, insider_share);
        noise(r, &mut w, &mut c);
        arbitrage(&mut w, &mut c);
        // Trend signal the bot can see on-chain: bins moved over the last 6 hours.
        hist.push(w.book.active);
        let window: usize = std::env::var("SIM_TREND_HOURS").ok().and_then(|v| v.parse().ok()).unwrap_or(6);
        let trigger: i32 = std::env::var("SIM_TREND_BINS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
        if hist.len() > window && w.book.active - hist[hist.len() - 1 - window] >= trigger {
            for h in holders.iter_mut().filter(|h| h.strategy == Strategy::RangeAuto) {
                w.trend_switch(h);
            }
        }
        w.crank();
        // Staggered entry: one or two small buyers per hour between hours 25 and 72,
        // so buyers don't move the price themselves (a fair comparison).
        if (BUYERS_ENTER..BUYERS_ENTER + 48).contains(&hour) && buyers.len() < 96 {
            let mut fresh = make_buyers(&mut w, &mut c.rng, buyers.len(), 2);
            buyers.append(&mut fresh);
        }
        if hour >= BUYERS_ENTER {
            keeper_settle(&mut w, &mut buyers);
            // DEFEND: a sustained downtrend (>=10 bins down over 6h) flips hybrids to defence.
            if hist.len() > 6 && w.book.active - hist[hist.len() - 7] <= -10 {
                // SIM_DEFEND=underwater: only defend buyers who are also below their entry.
                let underwater_only = std::env::var("SIM_DEFEND").as_deref() == Ok("underwater");
                let active = w.book.active;
                for b in buyers.iter_mut().filter(|b| defends(b.kind) && !b.defended && b.entry_hour < hour
                    && (!underwater_only || active <= b.entry_bin - 5)) {
                    defend(&mut w, b);
                }
            }
        }
        if hour % 24 == 0 {
            w.audit();
        }
        if (w.price() / c.fair - 1.0).abs() > 0.25 {
            lag += 1;
        }
        if std::env::var("SIM_TRACE").is_ok() && hour % 24 == 23 {
            let b = &w.book;
            let ask = b.bins.range(b.active..).find(|(_, x)| x.token > 0).map(|(k, x)| (*k, to_f(x.token)));
            let bid = b.bins.range(..=b.active).rev().find(|(_, x)| x.quote > 0).map(|(k, x)| (*k, to_f(x.quote)));
            eprintln!("day {:>2} fair {:.3e} book {:.3e} active {} next ask {:?} next bid {:?}", hour / 24, c.fair, w.price(), b.active, ask, bid);
        }
    }
    w.now = HOURS as u64 * HOUR;
    w.audit();
    let mut o = finish(r, &w, &c, &holders, p0, staked0, lag);
    (o.buyers, o.wheel_never_filled) = score_buyers(&w, &buyers, w.price().min(c.fair));
    o
}

const BUYERS_ENTER: u32 = 25; // after the 24h launch window
const BUYER_KINDS: usize = 8;

struct Buyer {
    name: String, kind: usize, deposit: u128, wallet_q: u128, wallet_t: u128, ever_filled: bool,
    entry_hour: u32, defended: bool, entry_bin: i32,
}

/// Does this kind use the hybrid "defend" switch?
fn defends(kind: usize) -> bool { kind == 6 || kind == 7 }

/// `count` new buyers of 0.5..2 NEAR each, cycling through the five strategies.
fn make_buyers(w: &mut World, rng: &mut Rng, first: usize, count: usize) -> Vec<Buyer> {
    let mut out = Vec::new();
    for i in first..first + count {
        let deposit = near(0.5 + 1.5 * rng.f());
        let hour = (w.now / HOUR) as u32;
        let mut b = Buyer { name: format!("b{i}"), kind: i % BUYER_KINDS, deposit, wallet_q: deposit, wallet_t: 0,
                            ever_filled: false, entry_hour: hour, defended: false, entry_bin: w.book.active };
        match b.kind {
            0 => {} // hold NEAR
            1 | 2 => {
                if let Some((kept, used, _)) = w.buy(&b.name, deposit, &[], 1.0) {
                    b.wallet_q -= used;
                    b.wallet_t += kept;
                    b.ever_filled = true;
                    if b.kind == 2 {
                        if let Ok((_, placed)) = w.book.place_ladder(&b.name, kept, &DEFAULT_LADDER, 0, w.now) {
                            b.wallet_t -= placed;
                            w.contract_t += placed;
                        }
                    }
                }
            }
            3 | 4 => {
                let (dips, preset): (&[(u32, u16)], u8) = if b.kind == 3 { (&[(9_000, 5_000), (8_000, 5_000)], 1) } else { (&[(9_500, 5_000), (9_000, 5_000)], 2) };
                if w.book.place_wheel(&b.name, deposit, dips, preset, w.now).is_ok() {
                    b.wallet_q -= deposit;
                    w.contract_q += deposit;
                }
            }
            _ => {
                // HYBRID: 25% spot now (kind 7: with a lock-in ladder on it), 75% as a tight wheel.
                let spot = deposit / 4;
                if let Some((kept, used, _)) = w.buy(&b.name, spot, &[], 1.0) {
                    b.wallet_q -= used;
                    b.wallet_t += kept;
                    b.ever_filled = true;
                    if b.kind == 7 {
                        if let Ok((_, placed)) = w.book.place_ladder(&b.name, kept, &DEFAULT_LADDER, 0, w.now) {
                            b.wallet_t -= placed;
                            w.contract_t += placed;
                        }
                    }
                }
                let rest = deposit - spot;
                if w.book.place_wheel(&b.name, rest, &[(9_500, 5_000), (9_000, 5_000)], 2, w.now).is_ok() {
                    b.wallet_q -= rest;
                    w.contract_q += rest;
                }
            }
        }
        out.push(b);
    }
    out
}

/// The keeper's job each hour: settle every buyer position with something to do
/// (wheel bids re-arm into sell liquidity; filled sells pay out to the wallet).
fn keeper_settle(w: &mut World, buyers: &mut [Buyer]) {
    for id in w.book.settleable(1_000) {
        let Some(owner) = w.book.positions.get(&id).map(|p| p.owner.clone()) else { continue };
        let Some(b) = buyers.iter_mut().find(|b| b.name == owner) else { continue };
        if w.book.positions[&id].bid {
            b.ever_filled = true;
        }
        if let Ok(s) = w.book.settle(id, w.now) {
            b.wallet_q += s.quote;
            b.wallet_t += s.token;
            w.contract_q -= s.quote;
            w.contract_t -= s.token;
        }
    }
}

/// Defence mode: cancel waiting bids (NEAR back to the wallet), pull every coin
/// out of sell liquidity and bid drawers, and re-place ALL coins as one-way
/// sell liquidity just above the price (+5 and +10 bins), so the first bounce
/// gets them out. Profits already paid out are untouched.
fn defend(w: &mut World, b: &mut Buyer) {
    b.defended = true;
    let mine: Vec<u64> = w.book.positions.iter().filter(|(_, p)| p.owner == b.name).map(|(i, _)| *i).collect();
    for id in mine {
        if let Ok(wd) = w.book.withdraw(id, &b.name, w.now) {
            let (t, q) = (wd.token + wd.fee_token, wd.quote + wd.fee_quote);
            b.wallet_t += t; b.wallet_q += q;
            w.contract_t -= t; w.contract_q -= q;
        }
    }
    let coins = b.wallet_t;
    if coins == 0 {
        return;
    }
    let a = w.book.active;
    if let Ok(_ids) = w.book.lp_range(&b.name, a + 5, a + 10, 2, coins, 0, true, 0, w.now) {
        b.wallet_t = 0;
        w.contract_t += coins;
    }
}

fn score_buyers(w: &World, buyers: &[Buyer], p: f64) -> ([f64; 8], f64) {
    let mut sum = [0.0; 8];
    let mut n = [0usize; 8];
    let (mut wheels, mut never) = (0, 0);
    for b in buyers {
        let mut v = to_f(b.wallet_q) + to_f(b.wallet_t) * p;
        for (id, pos) in &w.book.positions {
            if pos.owner == b.name {
                if let Ok((t, q)) = w.book.position_assets(*id) {
                    v += to_f(q) + to_f(t) * p;
                }
            }
        }
        sum[b.kind] += v / to_f(b.deposit);
        n[b.kind] += 1;
        if b.kind == 3 || b.kind == 4 {
            wheels += 1;
            if !b.ever_filled { never += 1; }
        }
    }
    let avg = [0, 1, 2, 3, 4, 5, 6, 7].map(|k| if n[k] > 0 { sum[k] / n[k] as f64 } else { f64::NAN });
    (avg, if wheels > 0 { never as f64 / wheels as f64 } else { 0.0 })
}

fn make_holders(r: &Regime, w: &mut World, rng: &mut Rng, insider_share: f64) -> Vec<Holder> {
    let weights: Vec<f64> = (0..r.holders).map(|_| rng.f() + 0.2).collect();
    let total: f64 = weights.iter().sum();
    let mut out = Vec::new();
    for (i, wt) in weights.iter().enumerate() {
        let deposit = (1.0 - insider_share) * RAISED * wt / total;
        let tokens = near(SUPPLY / 2.0 * deposit / RAISED);
        let mut h = Holder { name: format!("h{i}"), strategy: STRATEGIES[i % 4], deposit, wallet_t: tokens, wallet_q: 0, ladder: vec![], bids: vec![], range: (0, 0), switched: false };
        match h.strategy {
            Strategy::Spot => {}
            Strategy::LockIn => w.lp_range(&mut h, tokens / 2, true),
            Strategy::Range | Strategy::RangeAuto => w.lp_range(&mut h, tokens / 2, false),
        }
        let _ = (rng.f(), &DEFAULT_LADDER); // keep the random stream identical across strategy sets
        out.push(h);
    }
    out
}

/// Fair value: drift + noise, optionally pulled back toward an anchor (chop).
fn move_fair(r: &Regime, hour: u32, c: &mut Ctx) {
    let dt = 1.0 / 8_760.0;
    let drift = match r.phase2 { Some((start, d)) if hour >= start => d, _ => r.drift };
    let pull = r.revert * (c.anchor.ln() - c.fair.ln());
    let step = (drift + pull) * dt + r.vol * dt.sqrt() * c.rng.normal();
    c.fair *= step.exp();
}

fn event(r: &Regime, hour: u32, w: &mut World, c: &mut Ctx, holders: &mut [Holder], insider_share: f64) {
    match r.event {
        Event::InsiderDump { at, .. } if hour == at => {
            let tokens = near(SUPPLY / 2.0 * insider_share);
            let deposit = RAISED * insider_share;
            let got = w.sell(tokens, 1.0).map(|s| to_f(s.out)).unwrap_or(0.0); // dump at any price
            c.event_pnl = got - deposit;
            (c.fair, c.anchor) = (w.price(), w.price()); // the market re-prices to the crash
        }
        Event::TeamDump { share, at } if hour == at => {
            // Free tokens: never part of the contract's balances until sold.
            let got = w.sell(near(SUPPLY * share), 1.0).map(|s| to_f(s.out)).unwrap_or(0.0);
            c.event_pnl = got; // cost was zero
            (c.fair, c.anchor) = (w.price(), w.price());
        }
        Event::Snipe { near: amount } if hour == 0 => {
            if let Some((kept, _, ids)) = w.buy("sniper", near(amount), &[], 1.0) {
                c.stash = kept;
                c.event_pnl = -amount;
                let _ = ids; // locked ladder (if any) is valued at the end
            }
        }
        Event::Snipe { .. } if hour == 2 => {
            if let Some(s) = w.sell(std::mem::take(&mut c.stash), 1.0) {
                c.event_pnl += to_f(s.out);
            }
        }
        Event::BankRun { at } if hour == at => {
            c.fair *= 0.3;
            c.anchor = c.fair;
            arbitrage(w, c); // the crash hits the book first
            for h in holders.iter_mut() {
                w.close_all(h);
                if let Some(s) = w.sell(std::mem::take(&mut h.wallet_t), 1.0) {
                    h.wallet_q += s.out;
                    h.wallet_t += s.refund;
                }
                let staked = w.book.vault.stakers.get(&h.name).map(|s| s.stake).unwrap_or(0);
                if staked > 0 {
                    let _ = w.book.request_unstake(&h.name, staked, w.now);
                }
            }
        }
        Event::BankRun { at } if hour == at + 168 => {
            for h in holders.iter_mut() {
                if let Ok(t) = w.book.withdraw_unstaked(&h.name, w.now) {
                    w.contract_t -= t;
                    if let Some(s) = w.sell(t, 1.0) {
                        h.wallet_q += s.out;
                        h.wallet_t += s.refund;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Random traders. Lognormal sizes: mostly small, occasionally big.
fn noise(r: &Regime, w: &mut World, c: &mut Ctx) {
    for _ in 0..c.rng.poisson(r.noise_per_hour) {
        let size = r.noise_near * (0.8 * c.rng.normal() - 0.32).exp();
        if c.rng.f() < 0.5 {
            let bot = r.event == Event::Sandwich;
            let front = if bot { w.buy("bot", near(size), &[], 1.0) } else { None };
            w.buy("noise", near(size), &[], 0.05);
            if let Some((kept, used, _)) = front {
                let back = w.sell(kept, 1.0).map(|s| s.out).unwrap_or(0);
                c.event_pnl += to_f(back) - to_f(used);
            }
        } else {
            w.sell(near(size / w.price()), 0.05);
        }
    }
}

/// Arbitrageurs trade the book toward fair value while it pays more than fees.
fn arbitrage(w: &mut World, c: &mut Ctx) {
    // SIM_ARB_ROUNDS: how many 20-NEAR arbitrage trades per hour (default 40 = 800 NEAR/h).
    let rounds: u32 = std::env::var("SIM_ARB_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    for _ in 0..rounds {
        // A real bot only trades if the gap beats the CURRENT fee (dynamic or not).
        let fee = w.book.fee_at(w.book.active, w.book.active) as f64 / 10_000.0;
        let band = 2.0 * fee;
        let p = w.price();
        if p < c.fair * (1.0 - band) {
            if w.buy("arb", near(ARB_CHUNK), &[], 0.05).is_none() { break; }
        } else if p > c.fair * (1.0 + band) {
            if w.sell(near(ARB_CHUNK / p), 0.05).is_none() { break; }
        } else {
            break;
        }
    }
}

fn finish(r: &Regime, w: &World, c: &Ctx, holders: &[Holder], p0: f64, staked0: f64, lag: u32) -> Outcome {
    // Conservative mark: the lower of the book price and fair value, so a thin
    // book stuck at a silly price cannot make holders look rich.
    let p = w.price().min(c.fair);
    let mut sum = [0.0; 4];
    let mut n = [0usize; 4];
    let mut dividends = 0.0;
    for h in holders {
        let k = STRATEGIES.iter().position(|s| *s == h.strategy).unwrap();
        sum[k] += w.holder_value(h, p) / h.deposit;
        n[k] += 1;
        let (_, dq, dt) = w.book.vault.assets(&h.name);
        dividends += to_f(dq) + to_f(dt) * p;
    }
    let cohort = [0, 1, 2, 3].map(|k| if n[k] > 0 { sum[k] / n[k] as f64 } else { f64::NAN });
    let protocol: f64 = w.book.protocol_pos.values()
        .filter_map(|id| w.book.position_assets(*id).ok())
        .map(|(t, q)| to_f(q) + to_f(t) * p).sum();
    let hold = RAISED + SUPPLY / 2.0 * p;
    let mut event_pnl = c.event_pnl;
    if matches!(r.event, Event::Snipe { .. }) {
        // Add what the sniper's locked ladder is still worth.
        for (id, pos) in &w.book.positions {
            if pos.owner == "sniper" {
                let (t, q) = w.book.position_assets(*id).unwrap_or((0, 0));
                event_pnl += to_f(q) + to_f(t) * p;
            }
        }
    }
    Outcome {
        fair_mult: c.fair / p0, book_mult: w.price() / p0, cohort,
        staker_yield: if staked0 > 0.0 { dividends / staked0 } else { 0.0 },
        burned_pct: to_f(w.book.burned) / SUPPLY * 100.0,
        protocol_vs_hold: protocol / hold,
        volume: to_f(w.volume_q), event_pnl, lag_hours: lag, faults: w.faults.clone(),
        buyers: [f64::NAN; 8], wheel_never_filled: 0.0,
    }
}
