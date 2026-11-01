//! Market regimes: how the coin's "fair" price moves, how much random trading
//! happens, and which special events (crimes, runs, bots) occur.

/// A special event that happens during a run.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    None,
    /// Insiders who got `share` of the auction tokens dump everything at hour `at`.
    InsiderDump { share: f64, at: u32 },
    /// The dev team got `share` of total supply for FREE (a team allocation)
    /// and dumps all of it at hour `at`. This is the real "crime".
    TeamDump { share: f64, at: u32 },
    /// A bundler buys `near` NEAR in the first hour and dumps what it can at hour 2.
    Snipe { near: f64 },
    /// At hour `at` every holder tries to get out, and fair value falls to 30%.
    BankRun { at: u32 },
    /// A bot sandwiches every random buy: buys just before, sells just after.
    Sandwich,
}

#[derive(Clone, Debug)]
pub struct Regime {
    pub name: &'static str,
    pub what: &'static str,
    pub holders: usize,
    /// Fair price: drift per year (log), volatility per year, mean reversion speed.
    pub drift: f64,
    pub vol: f64,
    pub revert: f64,
    /// Optional second phase: (hour it starts, new drift).
    pub phase2: Option<(u32, f64)>,
    pub noise_per_hour: f64, // random trades per hour
    pub noise_near: f64,     // typical random trade size, NEAR
    pub event: Event,
    pub launch_lock: bool, // launch-window rule on or off
}

const DAYS30: f64 = 30.0 / 365.0;

fn base(name: &'static str, what: &'static str) -> Regime {
    Regime {
        name, what, holders: 60, drift: 0.0, vol: 1.5, revert: 0.0, phase2: None,
        noise_per_hour: 3.0, noise_near: 10.0, event: Event::None, launch_lock: true,
    }
}

pub fn all() -> Vec<Regime> {
    vec![
        Regime { holders: 8, noise_per_hour: 0.1, noise_near: 2.0, vol: 0.8, drift: (0.7f64).ln() / DAYS30,
                 ..base("ghost", "8 holders, almost no trading, slow fade to 0.7x") },
        Regime { revert: 20.0, ..base("steady", "chops around launch price, normal volume") },
        Regime { vol: 4.0, revert: 40.0, noise_per_hour: 6.0, noise_near: 20.0,
                 ..base("wild_chop", "violent swings around launch price, heavy volume") },
        Regime { drift: (10f64).ln() / DAYS30, noise_per_hour: 4.0, noise_near: 20.0,
                 ..base("moon", "trends to ~10x over 30 days") },
        Regime { drift: (5f64).ln() / (7.0 / 365.0), phase2: Some((168, (0.02f64).ln() / (23.0 / 365.0))),
                 noise_per_hour: 4.0, noise_near: 20.0, ..base("pump_dump", "5x in a week, then bleeds to ~0.1x") },
        Regime { holders: 30, drift: (0.2f64).ln() / DAYS30, noise_per_hour: 1.0, noise_near: 5.0, vol: 1.0,
                 ..base("slow_bleed", "fades to ~0.2x, thin trading") },
        Regime { event: Event::InsiderDump { share: 0.4, at: 48 }, noise_per_hour: 2.0,
                 ..base("crimed", "insiders own 40% of the auction and dump it on day 2") },
        Regime { event: Event::TeamDump { share: 0.2, at: 48 }, noise_per_hour: 2.0,
                 ..base("rugged_team", "dev team holds 20% extra supply for free and dumps it on day 2") },
        Regime { event: Event::Snipe { near: 3_000.0 }, revert: 20.0,
                 ..base("sniped", "bundler buys 3,000 NEAR at launch, dumps at hour 2 (launch lock ON)") },
        Regime { event: Event::Snipe { near: 3_000.0 }, revert: 20.0, launch_lock: false,
                 ..base("sniped_nolock", "same bundler, launch lock OFF") },
        Regime { event: Event::BankRun { at: 360 }, revert: 20.0,
                 ..base("bank_run", "day 15: fair value -70%, every holder rushes out") },
        Regime { event: Event::Sandwich, revert: 20.0, noise_per_hour: 6.0, noise_near: 20.0,
                 ..base("sandwich", "a bot sandwiches every random buy") },
    ]
}
