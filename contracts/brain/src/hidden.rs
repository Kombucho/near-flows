//! Hidden flows, as vault/src/hidden.rs: the brain stores only each node's
//! input token and a fingerprint sha256(salt || borsh(node)); a node is
//! revealed only when it runs (`step_hidden`). The fingerprint is byte-identical
//! to the vault's (same Recipe borsh, pinned vector below), so the keeper's
//! TypeScript and existing hidden plans work unchanged.

use flow::{Ledger, Node, Token};
use near_sdk::{env, require, AccountId};

use crate::{FlowRec, Recipe, Reply, Talk, NEAR};

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

impl Talk {
    pub(crate) fn save_hidden_flow(&mut self, name: String, inputs: Vec<Token>, fingerprints: Vec<String>) -> u32 {
        require!(name.len() <= 60, "name too long");
        require!(!inputs.is_empty() && inputs.len() <= flow::MAX_NODES, "1 to 32 nodes");
        require!(inputs.len() == fingerprints.len(), "one fingerprint per node");
        check_fingerprints(&fingerprints);
        for t in &inputs {
            let ok = t == NEAR || t == flow::NO_INPUT || crate::remote::is_remote(t) || t.parse::<AccountId>().ok().is_some_and(|a| self.trusted(&a).is_some());
            require!(ok, format!("untrusted: contract {t}"));
        }
        self.add_flow(FlowRec { name, ledger: Ledger::new(inputs.len()), nodes: Vec::new(), inputs, hidden: fingerprints })
    }

    pub(crate) fn hide_flow(&mut self, flow_id: u32, fingerprints: Vec<String>) {
        self.idle();
        check_fingerprints(&fingerprints);
        let f = self.flow_mut(flow_id);
        require!(!f.is_hidden(), "already hidden");
        require!(fingerprints.len() == f.inputs.len(), "one fingerprint per node");
        f.nodes.clear();
        f.hidden = fingerprints;
        self.emit("flow_hidden", &format!(r#""flow":{flow_id}"#));
    }

    pub(crate) fn reveal_flow(&mut self, flow_id: u32, nodes: Vec<Node<Recipe>>, salts: Vec<String>) {
        self.idle();
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
            self.check_node(n);
        }
        let f = self.flow_mut(flow_id);
        f.nodes = nodes;
        f.hidden.clear();
        self.emit("flow_revealed", &format!(r#""flow":{flow_id}"#));
    }

    pub(crate) fn step_hidden(&mut self, sender: &AccountId, flow_id: u32, node: u16, reveal: Node<Recipe>, salt: String) -> Reply {
        let f = self.flow(flow_id);
        require!(f.is_hidden(), "this flow is open: use step");
        let print = f.hidden.get(node as usize).unwrap_or_else(|| env::panic_str("no such node"));
        require!(fingerprint(&reveal, &salt) == *print, "doesn't match the node's fingerprint");
        if let Err(e) = flow::validate_node(node, &reveal, &f.inputs) {
            env::panic_str(&format!("bad node: {e:?}"));
        }
        self.begin_step(sender, flow_id, node, reveal, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vault's pinned vector (vault/src/tests.rs): the brain's Recipe must
    /// encode byte for byte like the vault's, or every saved hidden plan breaks.
    #[test]
    fn the_fingerprint_matches_the_vaults_pinned_vector() {
        let node: Node<Recipe> = near_sdk::serde_json::from_str(include_str!("../../vault/tests-data/fingerprint-node.json")).unwrap();
        let p = fingerprint(&node, "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff");
        assert_eq!(p, "ad13adedfaed79107498ceb3bb79901e8dff115529609033291981e71bd0d36b");
        let remote: Node<Recipe> = near_sdk::serde_json::from_str(include_str!("../../vault/tests-data/fingerprint-remote-node.json")).unwrap();
        let p = fingerprint(&remote, "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff");
        assert_eq!(p, "cfb851d722d274c36161359966c9a849ee9b7a5a947156f6acd0f7aa2acfe35c");
    }
}
