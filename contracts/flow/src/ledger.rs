//! A flow's running state: each node's inbox, the step in flight, failures.
//!
//! The vault's free balance lives in the contract; methods here say what to
//! take from it (`fund`) or give back to it (the Vec<(Token, u128)> returns).

use std::collections::BTreeMap;

#[cfg(feature = "borsh")]
use borsh::{BorshDeserialize, BorshSerialize};
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{entries, inputs, Action, Error, Node, Token, Trigger, BPS, MAX_FAILURES, NO_INPUT};

#[cfg_attr(feature = "borsh", derive(BorshSerialize, BorshDeserialize))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NodeState {
    /// Input waiting to be used.
    #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
    pub inbox: u128,
    /// Input taken by the step in flight.
    #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
    pub running: u128,
    pub runs: u32,
    pub last_run_ns: u64,
    pub failures: u8,
    /// Everything this node has handed out so far (for the canvas).
    #[cfg_attr(feature = "serde", serde(with = "crate::u128_str"))]
    pub out_total: u128,
}

#[cfg_attr(feature = "borsh", derive(BorshSerialize, BorshDeserialize))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(crate = "serde"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ledger {
    pub nodes: Vec<NodeState>,
    /// The node whose step is in flight, if any.
    pub busy: Option<u16>,
}

impl Ledger {
    pub fn new(len: usize) -> Self {
        Self { nodes: vec![NodeState::default(); len], busy: None }
    }

    fn state(&mut self, i: u16) -> Result<&mut NodeState, Error> {
        self.nodes.get_mut(i as usize).ok_or(Error::NoSuchNode(i))
    }

    /// Put `amount` (already taken from the vault's free balance) into entry node `i`.
    pub fn fund<A>(&mut self, flow: &[Node<A>], i: u16, amount: u128) -> Result<(), Error> {
        if !entries(flow).contains(&i) {
            return Err(Error::NotEntry(i));
        }
        self.fund_node(&flow[i as usize].input, i, amount)
    }

    /// Fund node `i`, whose input token is `input`, without the entry check (a
    /// hidden flow's routes aren't known, so its owner may fund any node).
    pub fn fund_node(&mut self, input: &Token, i: u16, amount: u128) -> Result<(), Error> {
        if input == NO_INPUT {
            return Err(Error::NoInput(i)); // collectors take no money
        }
        let s = self.state(i)?;
        s.inbox = s.inbox.checked_add(amount).ok_or(Error::Insufficient)?;
        Ok(())
    }

    /// Start node `i`'s step if its trigger allows. Returns the input amount to use.
    /// `price` is the current price for price triggers (the contract fetches it first).
    pub fn begin<A>(&mut self, flow: &[Node<A>], i: u16, now_ns: u64, price: Option<u128>, by_owner: bool) -> Result<u128, Error> {
        let node = flow.get(i as usize).ok_or(Error::NoSuchNode(i))?;
        self.begin_node(node, i, now_ns, price, by_owner)
    }

    /// `begin` for one node given on its own (a hidden flow's revealed node).
    pub fn begin_node<A>(&mut self, node: &Node<A>, i: u16, now_ns: u64, price: Option<u128>, by_owner: bool) -> Result<u128, Error> {
        if self.busy.is_some() {
            return Err(Error::Busy);
        }
        let s = self.state(i)?;
        if s.failures >= MAX_FAILURES {
            return Err(Error::Paused(i));
        }
        if node.input == NO_INPUT {
            // A collector: nothing to spend, only its schedule (or its owner) decides.
            match node.when {
                Trigger::Every { every_ns, .. } if s.runs > 0 && now_ns < s.last_run_ns.saturating_add(every_ns) => return Err(Error::NotYet(i)),
                Trigger::Manual if !by_owner => return Err(Error::OwnerOnly(i)),
                _ => {}
            }
            s.last_run_ns = now_ns;
            self.busy = Some(i);
            return Ok(0);
        }
        if s.inbox == 0 {
            return Err(Error::NoInput(i));
        }
        let amount = match node.when {
            Trigger::Now => s.inbox,
            Trigger::After { at_ns } => {
                if now_ns < at_ns {
                    return Err(Error::NotYet(i));
                }
                s.inbox
            }
            Trigger::Every { every_ns, chunk } => {
                if s.runs > 0 && now_ns < s.last_run_ns.saturating_add(every_ns) {
                    return Err(Error::NotYet(i));
                }
                chunk.min(s.inbox)
            }
            Trigger::PriceBelow { price: p } => match price {
                Some(x) if x <= p => s.inbox,
                _ => return Err(Error::NotYet(i)),
            },
            Trigger::PriceAbove { price: p } => match price {
                Some(x) if x >= p => s.inbox,
                _ => return Err(Error::NotYet(i)),
            },
            Trigger::Manual => {
                if !by_owner {
                    return Err(Error::OwnerOnly(i));
                }
                s.inbox
            }
        };
        s.inbox -= amount;
        s.running = amount;
        s.last_run_ns = now_ns;
        self.busy = Some(i);
        Ok(amount)
    }

    /// Finish node `i`'s step. `Ok(out)` = it handed out `out` of its output token
    /// (0 if it hands nothing out); `Err(left)` = it failed and `left` of the input
    /// came back. Returns what goes back to the vault's free balance.
    pub fn finish<A: Action>(&mut self, flow: &[Node<A>], i: u16, result: Result<u128, u128>) -> Result<Vec<(Token, u128)>, Error> {
        let node = flow.get(i as usize).ok_or(Error::NoSuchNode(i))?;
        self.finish_node(node, i, result)
    }

    /// `finish` for one node given on its own. Its routes must already have been
    /// checked against the flow (`validate_node`).
    pub fn finish_node<A: Action>(&mut self, node: &Node<A>, i: u16, result: Result<u128, u128>) -> Result<Vec<(Token, u128)>, Error> {
        if self.busy != Some(i) {
            return Err(Error::NotRunning(i));
        }
        self.busy = None;
        let s = self.state(i)?;
        let ran = std::mem::take(&mut s.running);
        let mut back = Vec::new();
        match result {
            Err(left) => {
                // The step failed: whatever came back returns to the inbox; anything
                // that did NOT come back is gone (the contract reports it).
                s.inbox += left.min(ran);
                // A collector failing usually means "nothing to collect yet": it spends
                // nothing and its schedule limits it, so it never pauses itself.
                if node.input != NO_INPUT {
                    s.failures += 1;
                }
            }
            Ok(out) => {
                s.failures = 0;
                s.runs += 1;
                s.out_total += out;
                if out > 0 {
                    let token = node.action.output(&node.input).expect("validated: routed nodes have output");
                    let mut routed = 0u128;
                    for r in &node.next {
                        if r.to as usize >= self.nodes.len() {
                            continue; // never happens after validate_node; the part stays free
                        }
                        let part = out * r.bps as u128 / BPS as u128;
                        routed += part;
                        self.nodes[r.to as usize].inbox += part;
                    }
                    if out > routed {
                        back.push((token, out - routed));
                    }
                }
            }
        }
        Ok(back)
    }

    /// `finish_node` for a step that worked but used only part of its input (a
    /// partial fill): the unused `left` goes back to the node's inbox, not to the
    /// vault's free balance.
    pub fn finish_node_partial<A: Action>(&mut self, node: &Node<A>, i: u16, out: u128, left: u128) -> Result<Vec<(Token, u128)>, Error> {
        let ran = self.nodes.get(i as usize).map_or(0, |s| s.running);
        let back = self.finish_node(node, i, Ok(out))?;
        self.state(i)?.inbox += left.min(ran);
        Ok(back)
    }

    /// Pause a node until its owner resumes it (e.g. it got less out than its minimum).
    pub fn pause(&mut self, i: u16) -> Result<(), Error> {
        self.state(i)?.failures = MAX_FAILURES;
        Ok(())
    }

    /// Owner un-pauses a node after three failures.
    pub fn resume(&mut self, i: u16) -> Result<(), Error> {
        self.state(i)?.failures = 0;
        Ok(())
    }

    /// Stop the flow: every inbox goes back to the free balance.
    pub fn drain<A>(&mut self, flow: &[Node<A>]) -> Result<Vec<(Token, u128)>, Error> {
        self.drain_inputs(&inputs(flow))
    }

    /// `drain`, knowing only each node's input token.
    pub fn drain_inputs(&mut self, inputs: &[Token]) -> Result<Vec<(Token, u128)>, Error> {
        if self.busy.is_some() {
            return Err(Error::Busy);
        }
        let mut back = Vec::new();
        for (s, t) in self.nodes.iter_mut().zip(inputs) {
            let a = std::mem::take(&mut s.inbox);
            if a > 0 {
                back.push((t.clone(), a));
            }
        }
        Ok(back)
    }

    /// What this flow is holding, by token (inboxes + the step in flight).
    pub fn holdings<A>(&self, flow: &[Node<A>]) -> BTreeMap<Token, u128> {
        self.holdings_inputs(&inputs(flow))
    }

    /// `holdings`, knowing only each node's input token.
    pub fn holdings_inputs(&self, inputs: &[Token]) -> BTreeMap<Token, u128> {
        let mut m = BTreeMap::new();
        for (s, t) in self.nodes.iter().zip(inputs) {
            let a = s.inbox + s.running;
            if a > 0 {
                *m.entry(t.clone()).or_default() += a;
            }
        }
        m
    }
}
