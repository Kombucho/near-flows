//! Flows: a user's investing steps wired together, like an n8n canvas.
//!
//! A flow is a list of nodes. Each node takes one token in, does one thing
//! (swap on Rhea, stake with LiNEAR, place a dip buy...) and may hand a token
//! out, which is split along its routes to later nodes. Whatever isn't routed
//! goes back to the vault's free balance, which only the owner can withdraw.
//!
//! This crate is the bookkeeping only: which node may run, with how much,
//! and where its output goes. What a node *does* is the contract's business
//! (the action type `A`). Keeping it pure means we can test every rule here.
//!
//! Safety rules enforced here:
//! - Routes only point FORWARD (to a higher node index), so a flow can never loop.
//! - A route's target must take exactly the token its source hands out.
//! - One step at a time: a running node holds its input aside until it finishes.
//!   A failed step gives the input back; three failures in a row pause the node.
//! - Nothing is created or lost: free + inboxes + running = what the vault holds
//!   for this flow (checked by `holdings`, tested in conservation tests).

mod ledger;
#[cfg(feature = "serde")]
pub mod u128_str;
#[cfg(test)]
mod tests;

pub use ledger::Ledger;

#[cfg(feature = "borsh")]
use borsh::{BorshDeserialize, BorshSerialize};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A token: "near" for native NEAR, otherwise the NEP-141 contract account id.
pub type Token = String;

/// The input of a COLLECTOR node: it spends nothing and only collects (claim
/// an idea's coin, settle positions). It runs on a schedule (`every`) or by hand.
pub const NO_INPUT: &str = "none";

pub const MAX_NODES: usize = 32;
pub const MAX_ROUTES: usize = 8;
pub const BPS: u32 = 10_000;
/// Failures in a row before a node pauses and needs its owner.
pub const MAX_FAILURES: u8 = 3;
/// Shortest repeat interval (a DCA can't hammer the keeper).
pub const MIN_EVERY_NS: u64 = 60 * 1_000_000_000;

/// What a node does, as far as the bookkeeping needs to know.
pub trait Action {
    /// The token this action hands out when it gets `input`, or None if it
    /// hands nothing out right away (e.g. it opens a position).
    fn output(&self, input: &Token) -> Option<Token>;
    /// Can this action take `input` at all?
    fn accepts(&self, input: &Token) -> bool;
}

#[cfg_attr(feature = "borsh", derive(BorshSerialize, BorshDeserialize))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Route {
    pub to: u16,
    pub bps: u16,
}

/// When a node may run (besides having input).
#[cfg_attr(feature = "borsh", derive(BorshSerialize, BorshDeserialize))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde", tag = "when", rename_all = "snake_case"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trigger {
    /// As soon as it has input.
    Now,
    /// Not before this time.
    After { at_ns: u64 },
    /// Repeatedly: every `every_ns`, taking `chunk` of the inbox each time (DCA).
    Every {
        every_ns: u64,
        #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
        chunk: u128,
    },
    /// When the price (quote per token, scaled 1e24) is at or below `price`.
    PriceBelow {
        #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
        price: u128,
    },
    /// When the price is at or above `price`.
    PriceAbove {
        #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
        price: u128,
    },
    /// Only the owner may run it.
    Manual,
}

#[cfg_attr(feature = "borsh", derive(BorshSerialize, BorshDeserialize))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node<A> {
    pub input: Token,
    pub action: A,
    pub when: Trigger,
    #[cfg_attr(feature = "serde", serde(default))]
    pub next: Vec<Route>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    Empty,
    TooManyNodes,
    TooManyRoutes(u16),
    RouteBackwards { from: u16, to: u16 },
    RouteTokenMismatch { from: u16, to: u16 },
    RoutesOverfull(u16),
    NothingToRoute(u16),
    BadInput(u16),
    BadTrigger(u16),
    NoSuchNode(u16),
    Busy,
    NotRunning(u16),
    Paused(u16),
    NotYet(u16),
    NoInput(u16),
    OwnerOnly(u16),
    NotEntry(u16),
    Insufficient,
}

/// Check a flow's shape once, when the owner saves it.
pub fn validate<A: Action>(nodes: &[Node<A>]) -> Result<(), Error> {
    if nodes.is_empty() {
        return Err(Error::Empty);
    }
    if nodes.len() > MAX_NODES {
        return Err(Error::TooManyNodes);
    }
    for (i, n) in nodes.iter().enumerate() {
        let i = i as u16;
        if !n.action.accepts(&n.input) {
            return Err(Error::BadInput(i));
        }
        if n.input == NO_INPUT && !matches!(n.when, Trigger::Every { .. } | Trigger::Manual) {
            return Err(Error::BadTrigger(i)); // a collector runs on a schedule or by hand, never in a loop
        }
        match n.when {
            Trigger::Every { every_ns, chunk } if every_ns < MIN_EVERY_NS || chunk == 0 => return Err(Error::BadTrigger(i)),
            Trigger::PriceBelow { price } | Trigger::PriceAbove { price } if price == 0 => return Err(Error::BadTrigger(i)),
            _ => {}
        }
        if n.next.len() > MAX_ROUTES {
            return Err(Error::TooManyRoutes(i));
        }
        let out = n.action.output(&n.input);
        if !n.next.is_empty() && out.is_none() {
            return Err(Error::NothingToRoute(i));
        }
        let mut total = 0u32;
        for r in &n.next {
            if r.to <= i {
                return Err(Error::RouteBackwards { from: i, to: r.to });
            }
            let target = nodes.get(r.to as usize).ok_or(Error::NoSuchNode(r.to))?;
            if Some(&target.input) != out.as_ref() || target.input == NO_INPUT {
                return Err(Error::RouteTokenMismatch { from: i, to: r.to });
            }
            total += r.bps as u32;
        }
        if total > BPS {
            return Err(Error::RoutesOverfull(i));
        }
    }
    Ok(())
}

/// Nodes nothing routes into: where the owner puts money in.
pub fn entries<A>(nodes: &[Node<A>]) -> Vec<u16> {
    let mut fed = vec![false; nodes.len()];
    for n in nodes {
        for r in &n.next {
            if let Some(f) = fed.get_mut(r.to as usize) {
                *f = true;
            }
        }
    }
    (0..nodes.len() as u16).filter(|&i| !fed[i as usize]).collect()
}
