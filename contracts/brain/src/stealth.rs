//! Stealth vaults: the owner is an Ed25519 KEY. The
//! owner signs commands off chain; anyone (normally the keeper) submits them
//! to the vault's `exec` door; the brain checks the signature and runs the
//! command as the owner. A stealth vault is its own owner (`owner` = the
//! vault's id), and the only way a request reaches the brain with the vault
//! itself as sender is this one: a relay never calls itself.
//!
//! The signed text is byte-identical to the vault's (pinned below), so the
//! keeper's stealth.ts works unchanged:
//!
//!   near-flows stealth v1
//!   vault: <vault id>
//!   nonce: <u64>
//!   expires_ns: <u64>
//!   tgas: <u64>
//!   method: <method>
//!   args: <JSON args, verbatim>
//!
//! The nonce is burned in this round; the command runs in the NEXT round
//! (`on_vault` Exec), so a command that then fails can't be replayed later,
//! exactly like the vault's self-call. The signed gas is what that round gets.

use near_sdk::{env, near, require, CurveType, Gas, PublicKey};

use crate::wire::Answer;
use crate::{Next, Reply, Req, Talk};

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Stealth {
    pub key: [u8; 32],
    pub nonce: u64,
}

/// The exact bytes the owner signs.
pub fn message(vault: &str, nonce: u64, expires_ns: u64, tgas: u64, method: &str, args: &str) -> String {
    format!("near-flows stealth v1\nvault: {vault}\nnonce: {nonce}\nexpires_ns: {expires_ns}\ntgas: {tgas}\nmethod: {method}\nargs: {args}")
}

pub(crate) fn ed25519(key: &PublicKey) -> [u8; 32] {
    require!(key.curve_type() == CurveType::ED25519, "the owner key must be ed25519");
    key.as_bytes()[1..].try_into().unwrap()
}

impl Talk {
    pub(crate) fn exec(&mut self, method: String, args: String, nonce: u64, expires_ns: u64, tgas: u64, signature: String) -> Answer {
        let s = self.vs.stealth.clone().unwrap_or_else(|| env::panic_str("not a stealth vault"));
        let sig: [u8; 64] = hex::decode(&signature).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("signature: 64 bytes, hex"));
        require!(env::ed25519_verify(&sig, message(self.vault.as_str(), nonce, expires_ns, tgas, &method, &args), &s.key), "bad signature");
        require!(env::block_timestamp() <= expires_ns, "expired");
        require!(nonce > s.nonce, "nonce already used");
        require!(!method.is_empty() && method.len() <= 64 && !method.contains('\n'), "bad method");
        require!(!NOT_COMMANDS.contains(&method.as_str()), "not a command");
        // The relayer can't starve the command: its round must get the signed gas.
        require!(self.ctx.gas_left >= Gas::from_tgas(tgas + crate::RELAY_TGAS).as_gas(), format!("attach at least {} Tgas", tgas + crate::RELAY_TGAS + crate::HANDLE_TGAS + crate::X_TGAS));
        self.vs.stealth = Some(Stealth { key: s.key, nonce });
        let next = near_sdk::borsh::to_vec(&Next::Exec { method, args, tgas }).unwrap();
        Answer::default().then("on_vault", next, Gas::from_tgas(tgas))
    }

    /// The command, as the owner. Its nonce is already burned.
    pub(crate) fn run_command(&mut self, method: String, args: String) -> Reply {
        require!(!NOT_COMMANDS.contains(&method.as_str()), "not a command");
        let args: near_sdk::serde_json::Value = near_sdk::serde_json::from_str(&args).unwrap_or_else(|_| env::panic_str("args: JSON"));
        let req: Req = near_sdk::serde_json::from_value(near_sdk::serde_json::json!({ method: args })).unwrap_or_else(|e| env::panic_str(&format!("bad command: {e}")));
        let owner = self.vs.owner.clone();
        self.request(owner, req)
    }

    pub(crate) fn set_owner_key(&mut self, owner_key: PublicKey) {
        let s = self.vs.stealth.clone().unwrap_or_else(|| env::panic_str("not a stealth vault"));
        self.vs.stealth = Some(Stealth { key: ed25519(&owner_key), nonce: s.nonce });
    }
}

/// Requests a signed command may never be: making vaults, signing again, hooks.
const NOT_COMMANDS: &[&str] = &["init", "init_stealth", "exec", "nft_on_transfer", "mt_on_transfer"];

#[cfg(test)]
mod tests {
    use super::*;

    // The pinned vector, signed by the keeper's node:crypto (scripts/keeper/src/stealth.test.ts).
    const PIN_TEXT: &str = "near-flows stealth v1\nvault: k7xq2m.vaults.near\nnonce: 1762400000000000000\nexpires_ns: 1762400600000000000\ntgas: 50\nmethod: withdraw_to\nargs: {\"token\":\"near\",\"amount\":\"1000\",\"to\":\"fresh-7f3a.near\",\"via_intents\":false}";
    const PIN_KEY: &str = "ed25519:FAe4sisG95oZ42w7buUn5qEE4TAnfTTFPiguZUHmhiF";
    const PIN_SIG: &str = "342a99949d4e2c41a51f4b5e8dc5b85519f0215d916ef5b9387e68392197c8f487903f25305dbe161e6ae895fd70276a4340bd3d73b0cee57db67d7b4e0ea601";
    const ARGS: &str = r#"{"token":"near","amount":"1000","to":"fresh-7f3a.near","via_intents":false}"#;

    #[test]
    fn the_signed_text_and_signature_match_the_keepers() {
        let text = message("k7xq2m.vaults.near", 1762400000000000000, 1762400600000000000, 50, "withdraw_to", ARGS);
        assert_eq!(text, PIN_TEXT);
        let key = ed25519(&PIN_KEY.parse().unwrap());
        let sig: [u8; 64] = hex::decode(PIN_SIG).unwrap().try_into().unwrap();
        assert!(env::ed25519_verify(&sig, &text, &key));
    }

    /// WHY: the full vault ran a signed command as a call to itself, so it had to
    /// keep every #[private] callback named on_* and refuse those names. Here a
    /// command can only ever be a `Req` (the request enum): the brain's own doors
    /// (handle, on_vault, on_read, pay) and the relay's (cb, x, new) are no `Req`,
    /// so a signed command can never fake a callback, whatever it is called.
    #[test]
    fn a_signed_command_can_never_name_a_callback_or_a_door() {
        for m in ["on_read", "on_vault", "handle", "pay", "cb", "x", "new", "on_measured"] {
            let r: Result<Req, _> = near_sdk::serde_json::from_value(near_sdk::serde_json::json!({ m: {} }));
            assert!(r.is_err(), "{m} must not be a request");
        }
    }
}
