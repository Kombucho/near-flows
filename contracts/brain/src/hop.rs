//! Hops: money leaving the vault's NEAR Intents balance for another chain
//! through the Intents POA bridge, always to the vault's OWN address there.
//!
//! A POA-bridged token (`sol.omft.near`, `base-0x8335….omft.near`, `hood.omft.near`…)
//! leaves Intents with the verifier's `ft_withdraw`, the token itself as the
//! receiver and the destination in the memo (near/intents, poa/token: "To
//! withdraw, users can call `ft_transfer` on the deployed token, pass token
//! itself as `receiver_id` and provide destination address in `memo` prefixed
//! with `WITHDRAW_TO:`"). The bridge pays the address in the memo on the other
//! chain, minus its withdrawal fee. Two codes answer that call on mainnet today
//! (read 2026-11-06): the EVM chains' omft tokens run the POA token, which burns;
//! the Solana ones were migrated to Omni Bridge's omni-token (0.4.3), which hands
//! the transfer, memo and all, to the bridge's withdraw relayer. Same call from
//! here; tests/hop.rs runs both codes against the real verifier.
//!
//! What the brain fixes, so the keeper can't:
//! - the destination: built here from the vault's own Solana or EVM address
//!   (read from the MPC signer at `sol_setup` / `evm_setup`), never an argument;
//! - the token: only ones the owner allowed (`intents_hop`), only POA tokens
//!   (`<chain>.omft.*` / `<chain>-<id>.omft.*`), each to its own kind of chain;
//! - the amount: at most the owner's cap per hop, counted against the spending
//!   limit `intents:<token>` like an Intents swap;
//! - what comes out, as a flow node: the remote-node machinery (remote.rs).
//!   The output on the other chain can't be seen from here, so it is
//!   KEEPER-REPORTED, between `amount - max_fee` (the owner's bound on the
//!   bridge's fee) and `amount` (same decimals on both sides: POA tokens mirror
//!   the origin token's).

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use crate::wire::Answer;
use crate::Talk;

pub const WITHDRAW_MEMO_PREFIX: &str = "WITHDRAW_TO:";
/// The verifier's ft_withdraw, the token's ft_transfer and the verifier's resolve
/// (15). The Intents docs attach 100 for a direct call; a whole hop burns 15-21
/// Tgas (sandbox, real verifier and token codes: tests/hop.rs), so 60 is ~3x.
pub(crate) const GAS_HOP: Gas = Gas::from_tgas(60);
/// on_vault after the withdrawal: reads its result, marks the step sent; burns
/// ~3 Tgas, 8 is a ~2.5x margin.
pub(crate) const GAS_HOP_SENT: Gas = Gas::from_tgas(8);
/// EVM chains the POA bridge serves with omft tokens, by the token's label, and
/// their chain ids (bridge `supported_tokens`, 2026-11-06).
const EVM_LABELS: &[(&str, u64)] = &[("eth", 1), ("base", 8453), ("arb", 42161), ("gnosis", 100), ("bera", 80094), ("hood", 4663)];

#[near(serializers = [borsh, json])]
#[serde(rename_all = "snake_case")]
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HopChain {
    Sol,
    Evm,
}

/// A token the owner lets leave Intents for the vault's own address on another chain.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Hop {
    /// The POA token, as on the verifier (no `nep141:`): e.g. `sol.omft.near`.
    pub token: AccountId,
    pub chain: HopChain,
    /// At most this much per hop (0 removes the hop).
    pub max: U128,
    /// The most the bridge may keep as its fee: a reported output below
    /// `amount - max_fee` is refused.
    pub max_fee: U128,
}

/// The token's chain label (`sol`, `base`…) if it is a POA bridge token: `<label>[-<id>].omft.<…>`.
pub fn poa_label(token: &str) -> Option<&str> {
    let (name, parent) = token.split_once('.')?;
    if !(parent == "omft.near" || parent.starts_with("omft.")) {
        return None;
    }
    let label = name.split_once('-').map_or(name, |(l, _)| l);
    (!label.is_empty()).then_some(label)
}

/// Which chain a POA token leaves to (and, on EVM, which chain id).
pub fn hop_chain_of(token: &str) -> Option<(HopChain, Option<u64>)> {
    let label = poa_label(token)?;
    if label == "sol" {
        return Some((HopChain::Sol, None));
    }
    EVM_LABELS.iter().find(|(l, _)| *l == label).map(|(_, id)| (HopChain::Evm, Some(*id)))
}

impl Talk {
    /// Owner: allow (or change, or with max 0 remove) a hop.
    pub(crate) fn intents_hop(&mut self, hop: Hop) {
        let (chain, _) = hop_chain_of(hop.token.as_str()).unwrap_or_else(|| env::panic_str("not a POA bridge token (<chain>.omft.near) of a chain hops serve"));
        require!(chain == hop.chain, "that token leaves to another kind of chain");
        require!(hop.max_fee.0 <= hop.max.0, "max_fee: at most max");
        let cfg = self.vs.intents.as_mut().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        cfg.hops.retain(|h| h.token != hop.token);
        if hop.max.0 > 0 {
            require!(cfg.hops.len() < 16, "too many hops");
            cfg.hops.push(hop);
        }
    }

    /// The hop for `token`, and the vault's own address it goes to.
    pub(crate) fn hop_route(&self, token: &AccountId) -> (Hop, String) {
        let cfg = self.vs.intents.as_ref().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        let hop = cfg.hops.iter().find(|h| h.token == *token).cloned().unwrap_or_else(|| env::panic_str("refused: no hop for that token"));
        let to = match hop.chain {
            HopChain::Sol => self.vs.sol.as_ref().and_then(|c| c.address.clone()).unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known: run sol_setup")),
            HopChain::Evm => self.vs.evm.as_ref().and_then(|c| c.address.clone()).unwrap_or_else(|| env::panic_str("the vault's EVM address isn't known: run evm_setup")),
        };
        (hop, to)
    }

    /// The withdrawal itself: checked, counted, and the vault's call to the verifier.
    /// Returns the call, the destination and the most the bridge may keep.
    pub(crate) fn hop_call(&mut self, token: &AccountId, amount: u128) -> (Answer, String, u128) {
        let (hop, to) = self.hop_route(token);
        require!(amount > 0 && amount <= hop.max.0, format!("refused: a hop moves 1 to {}", hop.max.0));
        self.charge(&format!("intents:{token}"), amount);
        let verifier = self.vs.intents.as_ref().unwrap().verifier.clone();
        let memo = format!("{WITHDRAW_MEMO_PREFIX}{to}");
        // No `msg`: the verifier uses ft_transfer and refunds the vault if the token refuses.
        let args = near_sdk::serde_json::json!({"token": token, "receiver_id": token, "amount": amount.to_string(), "memo": memo}).to_string();
        self.emit("hop", &format!(r#""token":"{token}","amount":"{amount}","to":"{to}","max_fee":"{}""#, hop.max_fee.0));
        (Answer::default().call(&verifier, "ft_withdraw", args.as_bytes(), 1, GAS_HOP), to, hop.max_fee.0)
    }

    /// Owner or keeper, outside any flow: hop `amount` of `token` to the vault's own address.
    pub(crate) fn intents_hop_out(&mut self, token: AccountId, amount: u128) -> Answer {
        self.hop_call(&token, amount).0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_poa_tokens_hop_and_each_to_its_own_kind_of_chain() {
        assert_eq!(hop_chain_of("sol.omft.near"), Some((HopChain::Sol, None)));
        assert_eq!(hop_chain_of("sol-5ce3bf3a31af18be40ba30f721101b4341690186.omft.near"), Some((HopChain::Sol, None)));
        assert_eq!(hop_chain_of("base-0x833589fcd6edb6e08f4c7c32d4f71b54bda02913.omft.near"), Some((HopChain::Evm, Some(8453))));
        assert_eq!(hop_chain_of("hood.omft.near"), Some((HopChain::Evm, Some(4663))));
        assert_eq!(hop_chain_of("sol.omft.test.near"), Some((HopChain::Sol, None)), "a sandbox's own omft account");
        // Not POA: a NEAR token, an Omni bridge deposit token, a chain hops don't serve, a look-alike.
        for t in ["wrap.near", "usdc.near", "sol-0xaad74c68eecfc9f8c5bdcea614f6167048c795ef.omdep.near", "btc.omft.near", "sol.omftx.near", "omft.near"] {
            assert_eq!(hop_chain_of(t), None, "{t}");
        }
    }
}
