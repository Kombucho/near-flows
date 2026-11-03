//! BACKTEST on real NEAR memecoins.
//!
//!   SIM_BACKTEST=sim-out/backtest cargo run --profile sim -p sim
//!
//! For each downloaded coin (real hourly prices in NEAR, from fetch.py), and for
//! each entry point (24h, 72h, 168h after its first trade), eight buyers enter
//! with 1 NEAR each, one per strategy. The coin's REAL price path drives the
//! outside price; arbitrage pulls our pool along it (assumption: our pool would
//! have tracked the real price). Scored at 7 and 21 days after entry.

use std::fs;

use engine::{Book, Config, DEFAULT_LADDER};

use crate::rng::Rng;
use crate::world::{near, to_f, Faults, World, HOUR};

const ENTRIES: [usize; 3] = [1, 6, 24];
const HORIZONS: [usize; 2] = [72, 168];
/// Rough NEAR/USD used only to turn real USD volume into NEAR volume.
const NEAR_USD: f64 = 4.3;
/// "Realistic" mark: coins of a coin with no trade in the last 72h are worth 0 (you can't sell).
const DEAD_AFTER_H: usize = 72;
pub const NAMES: [&str; 8] = ["hold NEAR", "market", "market+lock-in", "wheel 0.9/0.8", "wheel tight",
                              "hybrid 25/75", "hybrid+defend", "hybrid lock-in spot+defend"];

/// One coin's hourly series from its first trade for `len` hours: price
/// (forward-filled; frozen after the last trade) and REAL volume in NEAR (0 when no trades).
fn load(path: &str, len: usize) -> Option<(Vec<f64>, Vec<f64>)> {
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
    Some((price, vol))
}

/// Replay one coin from `entry` for `horizon` hours. Returns value/deposit per strategy.
/// Returns (optimistic, realistic) value/deposit per strategy.
fn replay(path: &[f64], volume: &[f64], entry: usize, horizon: usize, seed: u64) -> ([f64; 8], [f64; 8]) {
    let mut cfg = Config::standard(1, 30);
    cfg.dyn_k = 2;
    cfg.max_fee_bps = 500;
    cfg.launch_window_ns = 0; // entries are after launch day
    let book = Book::graduate(cfg, near(4_000.0), near(5e8), 230, 0);
    let mut w = World { book, now: 0, contract_q: near(4_000.0), contract_t: near(5e8), faults: Faults::default(), volume_q: 0 };
    let mut rng = Rng::new(seed);
    let p0 = w.price();
    let fair_at = |h: usize| p0 * path[h.min(path.len() - 1)] / path[0];
    let mut hist: Vec<i32> = Vec::new();
    let mut buyers: Vec<crate::run::Buyer> = Vec::new();
    for hour in 0..=(entry + horizon) {
        w.now = hour as u64 * HOUR;
        w.book.touch(w.now);
        let fair = fair_at(hour);
        crate::run::noise_volume(&mut w, &mut rng, volume[hour.min(volume.len() - 1)]);
        crate::run::arbitrage_to(&mut w, fair);
        if hour == entry {
            buyers = crate::run::make_buyers_one_each(&mut w, &mut rng);
        }
        if hour >= entry {
            crate::run::keeper_settle_pub(&mut w, &mut buyers);
            hist.push(w.book.active);
            if hist.len() > 6 && w.book.active - hist[hist.len() - 7] <= -10 {
                crate::run::defend_all(&mut w, &mut buyers, hour as u32);
            }
        }
        let _ = w.book.crank(w.now);
    }
    let end = entry + horizon;
    let mark = w.price().min(fair_at(end));
    let alive = volume[end.saturating_sub(DEAD_AFTER_H)..=end.min(volume.len() - 1)].iter().any(|v| *v > 0.0);
    let optimistic = crate::run::score_buyers_pub(&w, &buyers, mark).0;
    let realistic = if alive { optimistic } else { crate::run::score_buyers_pub(&w, &buyers, 0.0).0 };
    (optimistic, realistic)
}

pub fn main(dir: &str) {
    let manifest: serde_like::Manifest = serde_like::read(&format!("{dir}/manifest.json"));
    let mut results: Vec<Vec<[f64; 8]>> = vec![Vec::new(); HORIZONS.len()];
    let mut real: Vec<Vec<[f64; 8]>> = vec![Vec::new(); HORIZONS.len()];
    let mut coin_moves: Vec<Vec<f64>> = vec![Vec::new(); HORIZONS.len()];
    let mut used = 0;
    for (i, file) in manifest.files.iter().enumerate() {
        // Every coin is followed for the full window (dead coins included: frozen price, zero volume).
        let len = ENTRIES[2] + HORIZONS[1] + 1;
        let Some((path, volume)) = load(&format!("{dir}/{file}"), len) else { continue };
        if path[0] <= 0.0 {
            continue;
        }
        used += 1;
        for (hi, &h) in HORIZONS.iter().enumerate() {
            for &e in &ENTRIES {
                let (opt, rea) = replay(&path, &volume, e, h, (i * 31 + e) as u64);
                results[hi].push(opt);
                real[hi].push(rea);
                coin_moves[hi].push(path[e + h] / path[e]);
            }
        }
    }
    println!("BACKTEST on REAL NEAR launchpad coins: {used} coins (of {} sampled; dead ones included).", manifest.files.len());
    println!("8 buyers enter at 1h / 6h / 24h after a coin's first trade. Random trading = the coin's REAL hourly volume.");
    println!("Assumption: our pool tracks the real price via arbitrage. Values = end value / NEAR in.\n");
    for (hi, &h) in HORIZONS.iter().enumerate() {
        for (label, r) in [("OPTIMISTIC mark (coins at last price)", &results[hi]), ("REALISTIC mark (coins of coins with no trade in the last 72h = 0)", &real[hi])] {
        println!("{label}");
        let mv = &coin_moves[hi];
        let count = |f: &dyn Fn(f64) -> bool| mv.iter().filter(|x| f(**x)).count();
        println!("Horizon {} days: {} runs. The coins themselves: {} went >=2x, {} roughly flat (0.5-2x), {} fell below 0.5x, {} below 0.1x.",
            h / 24, r.len(), count(&|x| x >= 2.0), count(&|x| (0.5..2.0).contains(&x)), count(&|x| x < 0.5), count(&|x| x < 0.1));
        println!("| strategy | median | mean | beats market buy | worst 10% | best 10% |");
        println!("|---|---|---|---|---|---|");
        for k in 0..8 {
            let mut v: Vec<f64> = r.iter().map(|x| x[k]).filter(|x| x.is_finite()).collect();
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if v.is_empty() { continue; }
            let q = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
            let mean = v.iter().sum::<f64>() / v.len() as f64;
            let beats = r.iter().filter(|x| x[k] > x[1]).count() as f64 / r.len() as f64 * 100.0;
            println!("| {} | {:.2} | {:.2} | {:.0}% | {:.2} | {:.2} |", NAMES[k], q(0.5), mean, beats, q(0.1), q(0.9));
        }
        println!();
        }
    }
}

/// Minimal manifest reader (avoids a JSON dependency): pulls every "file" value.
mod serde_like {
    pub struct Manifest { pub files: Vec<String> }
    pub fn read(path: &str) -> Manifest {
        let text = std::fs::read_to_string(path).expect("manifest.json (run scripts/backtest/fetch.py first)");
        let files = text.split("\"file\": \"").skip(1).filter_map(|s| s.split('"').next().map(String::from)).collect();
        Manifest { files }
    }
}

#[allow(dead_code)]
fn _unused() -> (u128, f64) { (near(0.0), to_f(0)) }
#[allow(dead_code)]
const _L: usize = DEFAULT_LADDER.len();
