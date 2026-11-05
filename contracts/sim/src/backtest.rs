//! BACKTEST on real memecoins.
//!
//!   SIM_BACKTEST=sim-out/backtest cargo run --profile sim -p sim
//!
//! Input: a folder with manifest.json (listing "file" entries) and one CSV per
//! coin: `ts,price_near,volume_usd` (hourly or finer; any chain, prices in the
//! quote unit). Dead and rugged coins must be included.
//!
//! For each coin and entry point (1h, 6h, 24h after its first trade), one buyer
//! per strategy enters with ~1 NEAR. The coin's REAL price path is the outside
//! price. Two ways our pool can follow it:
//!   TRACKS:  arbitrageurs pull our pool to the real price every hour, however
//!            much trading that takes (optimistic: the arb trades pay LP fees too).
//!   CAPPED:  our pool can only trade the coin's REAL volume that hour. That
//!            volume first moves the price toward the real price; what is left
//!            is two-way trading. If the volume can't move it all the way, our
//!            price lags. Fees come only from real volume. This is the honest one.
//! Marks: coins of a coin with no trade in the last 72h are worth 0 (can't sell).

use std::fs;

use engine::{Book, Config};

use crate::rng::Rng;
use crate::run::BUYER_KINDS;
use crate::world::{near, World, HOUR};

const ENTRIES: [usize; 3] = [1, 6, 24];
const HORIZON: usize = 168;
/// Rough NEAR/USD used only to turn real USD volume into NEAR volume.
const NEAR_USD: f64 = 4.3;
const DEAD_AFTER_H: usize = 72;
/// Coins whose first trade is before this go in the first half (Sep 2026), the rest in the second (Oct).
const SPLIT_TS: i64 = 1_790_812_800; // 2026-10-01 00:00 UTC (override: SIM_SPLIT_TS)
const ARB_CHUNK: f64 = 20.0;
pub const NAMES: [&str; BUYER_KINDS] = ["hold NEAR", "market buy", "market+lock-in", "wheel 0.9/0.8", "wheel tight",
    "hybrid 25/75", "hybrid+defend", "hybrid lock-in+defend", "LP buy 0.7-1.4x", "LP buy 0.7-1.4x follow", "LP buy 0.9-1.15x follow"];
/// Strategies shown in the tables (the hybrids were already ruled out).
const SHOW: [usize; 7] = [0, 1, 2, 4, 8, 9, 10];

struct Coin { idx: usize, first_ts: i64, price: Vec<f64>, vol: Vec<f64> }

/// One coin's hourly series from its first trade for `len` hours: price
/// (forward-filled; frozen after the last trade) and REAL volume in NEAR.
fn load(path: &str, len: usize) -> Option<Coin> {
    let text = fs::read_to_string(path).ok()?;
    let mut pts: Vec<(i64, f64, f64)> = text.lines().skip(1).filter_map(|l| {
        let mut it = l.split(',');
        Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?))
    }).collect();
    pts.sort_by_key(|p| p.0);
    let first = pts.first()?.0;
    let (mut price, mut vol) = (vec![0.0; len], vec![0.0; len]);
    let mut cur = pts[0].1;
    let mut j = 0;
    for h in 0..len {
        let t = first + h as i64 * 3600;
        while j < pts.len() && pts[j].0 <= t {
            cur = pts[j].1;
            if pts[j].0 > t - 3600 { vol[h] += pts[j].2 / NEAR_USD; }
            j += 1;
        }
        price[h] = cur;
    }
    (price[0] > 0.0).then_some(Coin { idx: 0, first_ts: first, price, vol })
}

/// CAPPED mode: spend at most `budget` NEAR of trading moving our price toward
/// `fair`, then trade what is left both ways.
fn trade_capped(w: &mut World, rng: &mut Rng, fair: f64, mut budget: f64) {
    while budget > 0.01 {
        let fee = w.book.fee_at(w.book.active, w.book.active) as f64 / 10_000.0;
        let p = w.price();
        let chunk = ARB_CHUNK.min(budget);
        let done = if p < fair * (1.0 - 2.0 * fee) {
            w.buy("arb", near(chunk), &[], 0.05).is_some()
        } else if p > fair * (1.0 + 2.0 * fee) {
            w.sell(near(chunk / p), 0.05).is_some()
        } else {
            break;
        };
        if !done { break; }
        budget -= chunk;
    }
    crate::run::noise_volume(w, rng, budget);
}

/// Replay one coin from `entry` for HORIZON hours. Per strategy: (value / deposit, fees / deposit).
fn replay(c: &Coin, entry: usize, capped: bool, seed: u64) -> ([f64; BUYER_KINDS], [f64; BUYER_KINDS], bool) {
    let fee: u16 = std::env::var("SIM_FEE_BPS").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let mut cfg = Config::standard(1, fee);
    cfg.dyn_k = 2;
    cfg.max_fee_bps = 500;
    cfg.launch_window_ns = 0; // entries are after launch day
    let book = Book::graduate(cfg, near(4_000.0), near(5e8), 230, 0);
    let mut w = World { book, now: 0, contract_q: near(4_000.0), contract_t: near(5e8), faults: Default::default(), volume_q: 0 };
    let mut rng = Rng::new(seed);
    let p0 = w.price();
    let fair_at = |h: usize| p0 * c.price[h.min(c.price.len() - 1)] / c.price[0];
    let mut hist: Vec<i32> = Vec::new();
    let mut buyers = Vec::new();
    for hour in 0..=(entry + HORIZON) {
        w.now = hour as u64 * HOUR;
        w.book.touch(w.now);
        let fair = fair_at(hour);
        let vol = c.vol[hour.min(c.vol.len() - 1)];
        if capped {
            trade_capped(&mut w, &mut rng, fair, vol);
        } else {
            crate::run::noise_volume(&mut w, &mut rng, vol);
            crate::run::arbitrage_to(&mut w, fair);
        }
        if hour == entry {
            buyers = crate::run::make_buyers_one_each(&mut w, &mut rng);
        }
        if hour >= entry {
            crate::run::keeper_settle_pub(&mut w, &mut buyers);
            crate::run::follow_all(&mut w, &mut buyers);
            hist.push(w.book.active);
            if hist.len() > 6 && w.book.active - hist[hist.len() - 7] <= -10 {
                crate::run::defend_all(&mut w, &mut buyers, hour as u32);
            }
        }
        let _ = w.book.crank(w.now);
    }
    w.audit();
    if w.faults.insolvent > 0 || w.faults.unexpected > 0 {
        eprintln!("FAULT seed {seed} entry {entry} capped {capped}: insolvent {} unexpected {} first {:?}", w.faults.insolvent, w.faults.unexpected, w.faults.first);
    }
    let end = entry + HORIZON;
    let alive = c.vol[end.saturating_sub(DEAD_AFTER_H)..=end.min(c.vol.len() - 1)].iter().any(|v| *v > 0.0);
    // Mark at the lower of our price and the real price; dead coins at 0.
    let mark = if alive { w.price().min(fair_at(end)) } else { 0.0 };
    let ((value, _), fees) = crate::run::score_buyers_pub(&w, &buyers, mark);
    (value, fees, alive)
}

/// One run: which coin, which half, value and fees per strategy.
struct Run { coin: usize, late: bool, alive: bool, v: [f64; BUYER_KINDS], fee: [f64; BUYER_KINDS] }

pub fn main(dir: &str) {
    let files = manifest_files(&format!("{dir}/manifest.json"));
    let len = ENTRIES[2] + HORIZON + 1;
    let mut coins: Vec<Coin> = files.iter().filter_map(|f| load(&format!("{dir}/{f}"), len))
        .enumerate().map(|(i, mut c)| { c.idx = i; c }).collect();
    if let Some(every) = std::env::var("SIM_EVERY").ok().and_then(|v| v.parse::<usize>().ok()) {
        coins.retain(|c| c.idx % every == 0); // subsample for quick sensitivity runs
    }
    if let Some(only) = std::env::var("SIM_ONLY").ok().and_then(|v| v.parse::<usize>().ok()) {
        coins = coins.into_iter().skip(only).take(1).collect(); // debug one coin
    }
    let split: i64 = std::env::var("SIM_SPLIT_TS").ok().and_then(|v| v.parse().ok()).unwrap_or(SPLIT_TS);
    let early = coins.iter().filter(|c| c.first_ts < split).count();
    println!("BACKTEST: {} coins with a first trade ({} of {} files; dead ones included). {} first traded before the split date, {} after.",
        coins.len(), coins.len(), files.len(), early, coins.len() - early);
    println!("Each coin: buyers enter 1h / 6h / 24h after its first trade, one per strategy, scored 7 days later.");
    println!("Value = end value / NEAR in, coins marked at the lower of our price and the real price; coins of dead coins (no trade in the last 72h) = 0.\n");
    for capped in [true, false] {
        let mut runs = Vec::new();
        for (i, c) in coins.iter().enumerate() {
            for &e in &ENTRIES {
                let (v, fee, alive) = replay(c, e, capped, (c.idx * 31 + e) as u64);
                runs.push(Run { coin: i, late: c.first_ts >= split, alive, v, fee });
            }
        }
        report(if capped { "CAPPED: our pool trades only the coin's real volume (honest)" } else { "TRACKS: arbitrage pulls our pool to the real price every hour (optimistic)" },
            &runs, coins.len());
        let alive: Vec<Run> = runs.iter().filter(|r| r.alive).map(|r| Run { coin: r.coin, late: r.late, alive: true, v: r.v, fee: r.fee }).collect();
        let n = alive.iter().map(|r| r.coin).collect::<std::collections::BTreeSet<_>>().len();
        if capped && !alive.is_empty() {
            report(&format!("CAPPED, only runs where the coin still trades at the end ({} runs, {n} coins)", alive.len()), &alive, coins.len());
        }
    }
}

fn report(title: &str, runs: &[Run], n_coins: usize) {
    println!("## {title}\n");
    println!("| strategy | median | mean [95% CI] | P(loss) | worst 10% | best 10% | beats market buy | LP fees / NEAR in | 1st half mean, P(loss) | 2nd half mean, P(loss) |");
    println!("|---|---|---|---|---|---|---|---|---|---|");
    for &k in &SHOW {
        let all: Vec<&Run> = runs.iter().collect();
        let v = |rs: &[&Run]| -> Vec<f64> { rs.iter().map(|r| r.v[k]).filter(|x| x.is_finite()).collect() };
        let vals = v(&all);
        if vals.is_empty() { continue; }
        let (lo, hi) = bootstrap_mean(runs, k, n_coins);
        let beats = runs.iter().filter(|r| r.v[k] > r.v[1] + 1e-9).count() as f64 / runs.len() as f64 * 100.0;
        let fee = mean(&runs.iter().map(|r| r.fee[k]).filter(|x| x.is_finite()).collect::<Vec<_>>());
        let half = |late: bool| {
            let rs: Vec<&Run> = runs.iter().filter(|r| r.late == late).collect();
            let x = v(&rs);
            format!("{:.2}, {:.0}%", mean(&x), p_loss(&x) * 100.0)
        };
        println!("| {} | {:.2} | {:.2} [{:.2}-{:.2}] | {:.0}% | {:.2} | {:.2} | {:.0}% | {:.3} | {} | {} |",
            NAMES[k], quantile(&vals, 0.5), mean(&vals), lo, hi, p_loss(&vals) * 100.0,
            quantile(&vals, 0.1), quantile(&vals, 0.9), beats, fee, half(false), half(true));
    }
    println!();
}

fn mean(x: &[f64]) -> f64 {
    if x.is_empty() { f64::NAN } else { x.iter().sum::<f64>() / x.len() as f64 }
}

/// Share of runs that ended below what went in (0.5% tolerance for rounding).
fn p_loss(x: &[f64]) -> f64 {
    x.iter().filter(|v| **v < 0.995).count() as f64 / x.len().max(1) as f64
}

fn quantile(x: &[f64], q: f64) -> f64 {
    let mut v = x.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * q).round() as usize]
}

/// 95% CI of the mean, resampling whole COINS (a coin's 3 entries move together).
fn bootstrap_mean(runs: &[Run], k: usize, n_coins: usize) -> (f64, f64) {
    let mut by_coin: Vec<Vec<f64>> = vec![Vec::new(); n_coins];
    for r in runs {
        if r.v[k].is_finite() { by_coin[r.coin].push(r.v[k]); }
    }
    let mut rng = Rng::new(7);
    let mut means: Vec<f64> = (0..2_000).map(|_| {
        let (mut s, mut n) = (0.0, 0usize);
        for _ in 0..n_coins {
            let c = &by_coin[((rng.f() * n_coins as f64) as usize).min(n_coins - 1)];
            s += c.iter().sum::<f64>();
            n += c.len();
        }
        s / n.max(1) as f64
    }).collect();
    means.sort_by(|a, b| a.partial_cmp(b).unwrap());
    (means[50], means[1_949])
}

/// Every "file" value in the manifest (no JSON dependency needed).
fn manifest_files(path: &str) -> Vec<String> {
    let text = fs::read_to_string(path).expect("manifest.json (run scripts/backtest/fetch.py first)");
    text.split("\"file\": \"").skip(1).filter_map(|s| s.split('"').next().map(String::from)).collect()
}
