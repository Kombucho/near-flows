use std::collections::BTreeMap;

use crate::*;

/// Test actions: swap into a token, stake NEAR into LiNEAR, or open a position (no output).
#[derive(Clone, Debug, PartialEq, Eq)]
enum T {
    Swap(&'static str),
    Stake,
    Open,
    /// Spends nothing, collects USDC (e.g. claim proceeds).
    Collect,
}

impl Action for T {
    fn output(&self, _input: &Token) -> Option<Token> {
        match self {
            T::Swap(to) => Some(to.to_string()),
            T::Stake => Some("linear".into()),
            T::Open => None,
            T::Collect => Some("usdc".into()),
        }
    }
    fn accepts(&self, input: &Token) -> bool {
        match self {
            T::Stake => input == "near",
            T::Swap(to) => input != to,
            T::Open => true,
            T::Collect => input == NO_INPUT,
        }
    }
}

fn node(input: &str, action: T, when: Trigger, next: &[(u16, u16)]) -> Node<T> {
    Node { input: input.into(), action, when, next: next.iter().map(|&(to, bps)| Route { to, bps }).collect() }
}

/// NEAR -> half staked, half swapped to USDC; USDC -> 60% opens a position, 40% stays free.
fn flow() -> Vec<Node<T>> {
    vec![
        node("near", T::Swap("near"), Trigger::Now, &[]), // placeholder replaced below
    ]
    .into_iter()
    .take(0)
    .chain([
        node("near", T::Swap("usdc"), Trigger::Now, &[(2, 6_000)]),
        node("near", T::Stake, Trigger::Now, &[]),
        node("usdc", T::Open, Trigger::Now, &[]),
    ])
    .collect()
}

#[test]
fn a_well_formed_flow_validates_and_its_entries_are_found() {
    let f = flow();
    assert_eq!(validate(&f), Ok(()));
    assert_eq!(entries(&f), vec![0, 1]);
}

#[test]
fn flows_cannot_loop_or_mix_up_tokens() {
    let mut f = flow();
    f[2].next = vec![Route { to: 0, bps: 100 }];
    assert_eq!(validate(&f), Err(Error::NothingToRoute(2)), "a position node hands nothing out");
    let mut f = flow();
    f[0].next = vec![Route { to: 0, bps: 100 }];
    assert_eq!(validate(&f), Err(Error::RouteBackwards { from: 0, to: 0 }), "no self-loops");
    let mut f = flow();
    f[0].next = vec![Route { to: 1, bps: 100 }];
    assert_eq!(validate(&f), Err(Error::RouteTokenMismatch { from: 0, to: 1 }), "USDC can't feed a NEAR stake");
    let mut f = flow();
    f[0].next = vec![Route { to: 2, bps: 6_000 }, Route { to: 2, bps: 5_000 }];
    assert_eq!(validate(&f), Err(Error::RoutesOverfull(0)), "can't route 110%");
    let mut f = flow();
    f[0].next = vec![Route { to: 9, bps: 1 }];
    assert_eq!(validate(&f), Err(Error::NoSuchNode(9)));
    let f = vec![node("usdc", T::Stake, Trigger::Now, &[])];
    assert_eq!(validate(&f), Err(Error::BadInput(0)), "only NEAR can be staked");
    let f = vec![node("near", T::Stake, Trigger::Every { every_ns: 1, chunk: 5 }, &[])];
    assert_eq!(validate(&f), Err(Error::BadTrigger(0)), "a DCA can't fire every nanosecond");
    assert_eq!(validate::<T>(&[]), Err(Error::Empty));
    let big: Vec<_> = (0..33).map(|_| node("near", T::Stake, Trigger::Now, &[])).collect();
    assert_eq!(validate(&big), Err(Error::TooManyNodes));
}

#[test]
fn output_is_split_along_routes_and_the_rest_goes_back_to_free() {
    let f = flow();
    let mut l = Ledger::new(f.len());
    l.fund(&f, 0, 100).unwrap();
    assert_eq!(l.fund(&f, 2, 5), Err(Error::NotEntry(2)), "money enters only at entry nodes");
    assert_eq!(l.begin(&f, 0, 0, None, false), Ok(100));
    // The swap gave 250 USDC: 60% to node 2, 40% back to free.
    assert_eq!(l.finish(&f, 0, Ok(250)), Ok(vec![("usdc".into(), 100)]));
    assert_eq!(l.nodes[2].inbox, 150);
    assert_eq!(l.begin(&f, 2, 0, None, false), Ok(150));
    assert_eq!(l.finish(&f, 2, Ok(0)), Ok(vec![]), "a position node hands nothing back");
    assert_eq!(l.holdings(&f), BTreeMap::new());
}

#[test]
fn one_step_at_a_time() {
    let f = flow();
    let mut l = Ledger::new(f.len());
    l.fund(&f, 0, 10).unwrap();
    l.fund(&f, 1, 10).unwrap();
    l.begin(&f, 0, 0, None, false).unwrap();
    assert_eq!(l.begin(&f, 1, 0, None, false), Err(Error::Busy));
    assert_eq!(l.finish(&f, 1, Ok(1)), Err(Error::NotRunning(1)), "can't finish a step that isn't running");
    assert_eq!(l.drain(&f), Err(Error::Busy), "can't pull the rug from a step in flight");
}

#[test]
fn failures_give_the_input_back_and_three_in_a_row_pause_the_node() {
    let f = flow();
    let mut l = Ledger::new(f.len());
    l.fund(&f, 1, 30).unwrap();
    for _ in 0..3 {
        assert_eq!(l.begin(&f, 1, 0, None, false), Ok(30));
        l.finish(&f, 1, Err(30)).unwrap();
    }
    assert_eq!(l.begin(&f, 1, 0, None, false), Err(Error::Paused(1)));
    assert_eq!(l.nodes[1].inbox, 30);
    l.resume(1).unwrap();
    assert_eq!(l.begin(&f, 1, 0, None, false), Ok(30));
    // A failure that loses part of the input can't refund more than it took.
    l.finish(&f, 1, Err(1_000)).unwrap();
    assert_eq!(l.nodes[1].inbox, 30);
}

#[test]
fn triggers_wait_for_their_moment() {
    const MIN: u64 = 60_000_000_000;
    let f = vec![
        node("near", T::Stake, Trigger::After { at_ns: 100 }, &[]),
        node("near", T::Stake, Trigger::Every { every_ns: MIN, chunk: 4 }, &[]),
        node("usdc", T::Swap("near"), Trigger::PriceBelow { price: 50 }, &[]),
        node("usdc", T::Swap("near"), Trigger::PriceAbove { price: 80 }, &[]),
        node("near", T::Stake, Trigger::Manual, &[]),
    ];
    validate(&f).unwrap();
    let mut l = Ledger::new(f.len());
    for i in 0..5 {
        l.fund(&f, i, 10).unwrap();
    }
    assert_eq!(l.begin(&f, 0, 99, None, false), Err(Error::NotYet(0)));
    assert_eq!(l.begin(&f, 0, 100, None, false), Ok(10));
    l.finish(&f, 0, Ok(10)).unwrap();
    // DCA: 4 now, then nothing until a minute has passed, then 4, then the last 2.
    assert_eq!(l.begin(&f, 1, 1_000, None, false), Ok(4));
    l.finish(&f, 1, Ok(4)).unwrap();
    assert_eq!(l.begin(&f, 1, 1_000 + MIN - 1, None, false), Err(Error::NotYet(1)));
    assert_eq!(l.begin(&f, 1, 1_000 + MIN, None, false), Ok(4));
    l.finish(&f, 1, Ok(4)).unwrap();
    assert_eq!(l.begin(&f, 1, 1_000 + 2 * MIN, None, false), Ok(2));
    l.finish(&f, 1, Ok(2)).unwrap();
    assert_eq!(l.begin(&f, 1, 1_000 + 3 * MIN, None, false), Err(Error::NoInput(1)));
    // Price triggers need a price, and the right one.
    assert_eq!(l.begin(&f, 2, 0, None, false), Err(Error::NotYet(2)), "no price, no trade");
    assert_eq!(l.begin(&f, 2, 0, Some(51), false), Err(Error::NotYet(2)));
    assert_eq!(l.begin(&f, 2, 0, Some(50), false), Ok(10));
    l.finish(&f, 2, Ok(1)).unwrap();
    assert_eq!(l.begin(&f, 3, 0, Some(79), false), Err(Error::NotYet(3)));
    assert_eq!(l.begin(&f, 3, 0, Some(80), false), Ok(10));
    l.finish(&f, 3, Ok(1)).unwrap();
    // Manual: the keeper can't, the owner can.
    assert_eq!(l.begin(&f, 4, 0, None, false), Err(Error::OwnerOnly(4)));
    assert_eq!(l.begin(&f, 4, 0, None, true), Ok(10));
}

#[test]
fn draining_returns_every_inbox() {
    let f = flow();
    let mut l = Ledger::new(f.len());
    l.fund(&f, 0, 7).unwrap();
    l.fund(&f, 1, 3).unwrap();
    assert_eq!(l.drain(&f), Ok(vec![("near".into(), 7), ("near".into(), 3)]));
    assert_eq!(l.holdings(&f), BTreeMap::new());
}

/// Random funding, steps, outputs, failures and drains: the vault's books always balance.
/// "In" = funded + outputs; "out" = free returns + what steps consumed; held = holdings.
#[test]
fn nothing_is_created_or_lost() {
    let f = vec![
        node("near", T::Swap("usdc"), Trigger::Now, &[(1, 3_333), (2, 3_333)]),
        node("usdc", T::Swap("near"), Trigger::Now, &[(3, 9_999)]),
        node("usdc", T::Open, Trigger::Now, &[]),
        node("near", T::Stake, Trigger::Now, &[]),
    ];
    validate(&f).unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = |m: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % m
    };
    for _ in 0..200 {
        let mut l = Ledger::new(f.len());
        let mut inflow: BTreeMap<Token, u128> = BTreeMap::new();
        let mut outflow: BTreeMap<Token, u128> = BTreeMap::new();
        for _ in 0..60 {
            match rnd(5) {
                0 => {
                    let a = rnd(1_000_000) as u128 + 1;
                    l.fund(&f, 0, a).unwrap();
                    *inflow.entry("near".into()).or_default() += a;
                }
                1 | 2 => {
                    let i = rnd(4) as u16;
                    if let Ok(a) = l.begin(&f, i, 0, None, false) {
                        let input = f[i as usize].input.clone();
                        let result = if rnd(4) == 0 {
                            let left = rnd(a as u64 + 1) as u128;
                            *outflow.entry(input).or_default() += a - left; // lost in the failed step
                            Err(left)
                        } else {
                            *outflow.entry(input).or_default() += a; // consumed by the action
                            let out = if f[i as usize].action.output(&f[i as usize].input).is_some() { rnd(2_000_000) as u128 } else { 0 };
                            if let Some(t) = f[i as usize].action.output(&f[i as usize].input) {
                                *inflow.entry(t).or_default() += out;
                            }
                            Ok(out)
                        };
                        for (t, a) in l.finish(&f, i, result).unwrap() {
                            *outflow.entry(t).or_default() += a;
                        }
                    }
                }
                3 => {
                    if let Ok(back) = l.drain(&f) {
                        for (t, a) in back {
                            *outflow.entry(t).or_default() += a;
                        }
                    }
                }
                _ => {
                    let _ = l.resume(rnd(4) as u16);
                }
            }
            let held = l.holdings(&f);
            for t in ["near", "usdc", "linear"] {
                let i = inflow.get(t).copied().unwrap_or(0);
                let o = outflow.get(t).copied().unwrap_or(0);
                let h = held.get(t).copied().unwrap_or(0);
                assert_eq!(i, o + h, "token {t}: in {i} != out {o} + held {h}");
            }
        }
    }
}

#[test]
fn collectors_spend_nothing_run_on_schedule_and_pass_on_what_they_collect() {
    const MIN: u64 = 60_000_000_000;
    let f = vec![
        node(NO_INPUT, T::Collect, Trigger::Every { every_ns: MIN, chunk: 1 }, &[(1, 10_000)]),
        node("usdc", T::Swap("near"), Trigger::Now, &[]),
    ];
    validate(&f).unwrap();
    let mut l = Ledger::new(f.len());
    assert_eq!(l.fund(&f, 0, 5), Err(Error::NoInput(0)), "collectors take no money");
    assert_eq!(l.begin(&f, 0, 0, None, false), Ok(0));
    assert_eq!(l.finish(&f, 0, Ok(70)), Ok(vec![]));
    assert_eq!(l.nodes[1].inbox, 70, "what it collected flows on");
    assert_eq!(l.begin(&f, 0, MIN - 1, None, false), Err(Error::NotYet(0)), "not more often than its schedule");
    assert_eq!(l.begin(&f, 0, MIN, None, false), Ok(0));
    // A collector can't run in a loop ("now"), and nothing can be routed into one.
    let looping = vec![node(NO_INPUT, T::Collect, Trigger::Now, &[])];
    assert_eq!(validate(&looping), Err(Error::BadTrigger(0)));
    let into = vec![node("near", T::Swap(NO_INPUT), Trigger::Now, &[(1, 100)]), node(NO_INPUT, T::Collect, Trigger::Manual, &[])];
    assert_eq!(validate(&into), Err(Error::RouteTokenMismatch { from: 0, to: 1 }));
}
