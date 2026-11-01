//! Stress-test simulator.
//!
//!   cargo run --profile sim -p sim -- [runs] [base fee bps] [bins per side] [dynamic k] [max fee bps]
//!
//! Runs every regime `runs` times with different seeds, checks solvency every
//! simulated day, catches any panic, and prints what happened to holders.
//! All numbers are SIMULATION output, not predictions.

mod regime;
mod rng;
mod run;
mod world;

use std::panic::{catch_unwind, AssertUnwindSafe};

use run::{run, Outcome};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let runs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(300);
    let fee: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100);
    let bins: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(100);
    let dyn_k: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(0);
    let max_fee: u16 = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(500);
    if std::env::var("SIM_QUIET").is_ok() { std::panic::set_hook(Box::new(|_| {})); } // panics are counted
    println!("SIMULATION: {runs} runs x {} regimes, 30 days each, fee {fee} bps (dynamic k={dyn_k}, max {max_fee} bps), graduation liquidity over {bins} bins per side\n", regime::all().len());
    for r in regime::all() {
        println!("- {}: {}", r.name, r.what);
    }
    println!();
    println!("| regime | fair x | book x | spot | ladder | stake | ladder+stake | div yield | burned | protocol vs hold | event PnL (NEAR) | lag h | faults |");
    println!("|---|---|---|---|---|---|---|---|---|---|---|---|---|");
    let mut csv = String::from("regime,seed,fair_mult,book_mult,spot,ladder,stake,ladder_stake,div_yield,burned_pct,protocol_vs_hold,volume,event_pnl,lag_hours,insolvent,unexpected,panicked\n");
    let mut problems = Vec::new();
    let only = std::env::var("SIM_ONLY").ok();
    let regimes: Vec<_> = regime::all().into_iter().filter(|r| only.as_deref().map_or(true, |o| o == r.name)).collect();
    // One thread per regime; each returns its table row, CSV lines and problems.
    let results: Vec<(String, String, Vec<String>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = regimes.iter().map(|r| scope.spawn(move || run_regime(r, runs, fee, bins, dyn_k, max_fee))).collect();
        handles.into_iter().map(|h| h.join().expect("regime thread")).collect()
    });
    for (row, lines, probs) in results {
        println!("{row}");
        csv += &lines;
        problems.extend(probs);
    }
    std::fs::create_dir_all("sim-out").ok();
    let file = format!("sim-out/results_fee{fee}_bins{bins}_k{dyn_k}.csv");
    std::fs::write(&file, csv).expect("write csv");
    println!("\nCells: median [10th-90th percentile] across seeds. Holder columns = end value / NEAR put in.");
    println!("Per-run rows written to contracts/{file}");
    println!("\nProblems found: {}", problems.len());
    for p in problems.iter().take(15) {
        println!("  {p}");
    }
}

fn run_regime(r: &regime::Regime, runs: u64, fee: u16, bins: i32, dyn_k: u16, max_fee: u16) -> (String, String, Vec<String>) {
    let (mut outs, mut lines, mut problems, mut panics) = (Vec::new(), String::new(), Vec::new(), 0);
    for seed in 0..runs {
        match catch_unwind(AssertUnwindSafe(|| run(r, seed, fee, bins, dyn_k, max_fee))) {
            Ok(o) => {
                lines += &format!("{},{seed},{},{},{},{},{},{},{},{},{},{},{},{},{},{},0\n", r.name, o.fair_mult, o.book_mult,
                    o.cohort[0], o.cohort[1], o.cohort[2], o.cohort[3], o.staker_yield, o.burned_pct,
                    o.protocol_vs_hold, o.volume, o.event_pnl, o.lag_hours, o.faults.insolvent, o.faults.unexpected);
                if let Some(f) = &o.faults.first {
                    problems.push(format!("{} seed {seed}: {f}", r.name));
                }
                outs.push(o);
            }
            Err(e) => {
                panics += 1;
                let msg = e.downcast_ref::<String>().cloned().or(e.downcast_ref::<&str>().map(|s| s.to_string()));
                problems.push(format!("{} seed {seed}: PANIC {}", r.name, msg.unwrap_or_default()));
            }
        }
    }
    (row(r.name, &outs, panics), lines, problems)
}

fn q(mut v: Vec<f64>) -> String {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return "n/a".into();
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let at = |p: f64| v[((v.len() - 1) as f64 * p).round() as usize];
    format!("{} [{}-{}]", fmt(at(0.5)), fmt(at(0.1)), fmt(at(0.9)))
}

fn fmt(x: f64) -> String {
    if x.abs() >= 100.0 { format!("{x:.0}") } else if x.abs() >= 10.0 { format!("{x:.1}") } else { format!("{x:.2}") }
}

fn row(name: &str, o: &[Outcome], panics: u32) -> String {
    let col = |f: &dyn Fn(&Outcome) -> f64| q(o.iter().map(f).collect());
    let faults: u32 = o.iter().map(|x| x.faults.insolvent + x.faults.unexpected).sum::<u32>() + panics;
    format!("| {name} | {} | {} | {} | {} | {} | {} | {} | {}% | {} | {} | {} | {faults} |",
        col(&|x| x.fair_mult), col(&|x| x.book_mult),
        col(&|x| x.cohort[0]), col(&|x| x.cohort[1]), col(&|x| x.cohort[2]), col(&|x| x.cohort[3]),
        col(&|x| x.staker_yield * 100.0).replace(' ', "% ").replacen('[', "[", 1),
        col(&|x| x.burned_pct), col(&|x| x.protocol_vs_hold), col(&|x| x.event_pnl), col(&|x| x.lag_hours as f64))
}
