//! Hidden flows: the plan stays secret until it runs.
//!
//! Everything in a contract's state can be read by anyone, so a hidden flow
//! doesn't store its nodes at all. For each node the vault keeps only:
//! - its input token (what the vault holds for it: balances are public anyway),
//! - a fingerprint: sha256(salt || borsh(node)), with a fresh random 32-byte salt
//!   per node. Without the salt, nobody can confirm a guess, not even "it's the
//!   usual Rhea swap with a dip trigger".
//!
//! To run a node, the owner (or their keeper) hands over the node and its salt
//! (`step_hidden`). The vault checks the fingerprint, checks the node like any
//! saved node (trusted contracts and methods, routes forward to the right token)
//! and runs it. From then on that one node is public: it's in the transaction.
//! The rest of the flow stays hidden.
//!
//! A flow can switch either way:
//! - `reveal_flow`: hidden to open. Every node is checked against its fingerprint,
//!   so readers know they see the real flow.
//! - `hide_flow`: open to hidden. This only hides what comes next: the nodes it
//!   had are still in the chain's history.
//!
//! Who sees a hidden flow: its owner, and whoever runs its steps (their keeper).

use flow::{Ledger, Node, Token};
use near_sdk::{env, near, require, Promise};

use crate::{FlowRec, Recipe, Vault, VaultExt, NEAR};

/// A node's fingerprint: hex(sha256(salt || borsh(node))). `salt`: 32 bytes, hex.
pub fn fingerprint(node: &Node<Recipe>, salt: &str) -> String {
    let mut data = hex::decode(salt).ok().filter(|s| s.len() == 32).unwrap_or_else(|| env::panic_str("salt: 32 random bytes, hex"));
    data.extend(near_sdk::borsh::to_vec(node).unwrap_or_else(|_| env::panic_str("bad node")));
    hex::encode(env::sha256(&data))
}

fn check_fingerprints(prints: &[String]) {
    for p in prints {
        require!(p.len() == 64 && hex::decode(p).is_ok(), "a fingerprint is 32 bytes, hex");
    }
}

#[near]
impl Vault {
    /// Save a flow without showing it: one input token and one fingerprint per node.
    pub fn save_hidden_flow(&mut self, name: String, inputs: Vec<Token>, fingerprints: Vec<String>) -> u32 {
        self.assert_owner();
        require!(name.len() <= 60, "name too long");
        require!(!inputs.is_empty() && inputs.len() <= flow::MAX_NODES, "1 to 32 nodes");
        require!(inputs.len() == fingerprints.len(), "one fingerprint per node");
        check_fingerprints(&fingerprints);
        for t in &inputs {
            require!(t == NEAR || t == flow::NO_INPUT || crate::remote::is_remote(t) || self.trusted.iter().any(|x| x.contract.as_str() == t), format!("untrusted: contract {t}"));
        }
        self.add_flow(FlowRec { name, ledger: Ledger::new(inputs.len()), nodes: Vec::new(), inputs, hidden: fingerprints })
    }

    /// Hide an open flow from now on. Its nodes may change too (same input tokens):
    /// the new fingerprints are whatever the owner commits to.
    pub fn hide_flow(&mut self, flow_id: u32, fingerprints: Vec<String>) {
        self.assert_owner();
        self.assert_idle();
        check_fingerprints(&fingerprints);
        let f = self.flow_mut(flow_id);
        require!(!f.is_hidden(), "already hidden");
        require!(fingerprints.len() == f.inputs.len(), "one fingerprint per node");
        f.nodes.clear();
        f.hidden = fingerprints;
        self.emit("flow_hidden", &format!(r#"{{"flow":{flow_id}}}"#));
    }

    /// Show a hidden flow. Every node must match its fingerprint and pass the
    /// checks of `save_flow`.
    pub fn reveal_flow(&mut self, flow_id: u32, nodes: Vec<Node<Recipe>>, salts: Vec<String>) {
        self.assert_owner();
        self.assert_idle();
        let f = self.flow(flow_id);
        require!(f.is_hidden(), "already open");
        require!(nodes.len() == f.hidden.len() && salts.len() == nodes.len(), "every node, with its salt");
        for (i, (n, salt)) in nodes.iter().zip(&salts).enumerate() {
            require!(fingerprint(n, salt) == f.hidden[i], format!("node {i} doesn't match its fingerprint"));
        }
        require!(flow::inputs(&nodes) == f.inputs, "input tokens changed");
        if let Err(e) = flow::validate(&nodes) {
            env::panic_str(&format!("bad flow: {e:?}"));
        }
        for n in &nodes {
            if let Some(why) = self.untrusted(n) {
                env::panic_str(&format!("untrusted: {why}"));
            }
        }
        let f = self.flow_mut(flow_id);
        f.nodes = nodes;
        f.hidden.clear();
        self.emit("flow_revealed", &format!(r#"{{"flow":{flow_id}}}"#));
    }

    /// Run node `node` of a hidden flow, revealing just that node. Owner or keeper.
    pub fn step_hidden(&mut self, flow_id: u32, node: u16, reveal: Node<Recipe>, salt: String) -> Promise {
        let f = self.flow(flow_id);
        require!(f.is_hidden(), "this flow is open: use step");
        let print = f.hidden.get(node as usize).unwrap_or_else(|| env::panic_str("no such node"));
        require!(fingerprint(&reveal, &salt) == *print, "doesn't match the node's fingerprint");
        if let Err(e) = flow::validate_node(node, &reveal, &f.inputs) {
            env::panic_str(&format!("bad node: {e:?}"));
        }
        self.begin_step(flow_id, node, reveal, true)
    }

    /// A node's fingerprint, for apps that can't compute borsh themselves.
    /// Ask your own RPC node: whoever answers the view sees the node.
    pub fn fingerprint_of(&self, node: Node<Recipe>, salt: String) -> String {
        fingerprint(&node, &salt)
    }
}
