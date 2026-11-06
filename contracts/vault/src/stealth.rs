//! Stealth vaults: the owner is a KEY, not an account.
//!
//! A named vault's owner is a NEAR account, so everyone can see whose vault it is.
//! A stealth vault stores only an Ed25519 public key. The owner signs commands off
//! chain with that key; anyone (normally the keeper, acting as a relayer) submits
//! them with `exec`; the vault checks the signature itself and runs the command
//! as its owner. The owner's own wallet never sends a transaction here, so nothing
//! on chain points from their wallet to this vault.
//!
//! How "as its owner" works without touching every owner method: a stealth vault
//! is its OWN owner (`owner` = the vault's account id). `exec` makes a call from
//! the vault to itself, so the owner checks everywhere (`assert_owner`, and the
//! "owner or keeper" checks of steps, intents, Solana, EVM) pass for exactly that
//! call. Nothing else can make the vault call itself with a method of its choice:
//! a vault never trusts itself (recipes, `act`), and keepers pass numbers, never
//! method names. Callbacks (`on_*`) are refused by `exec`, so a signed command
//! can't fake a promise result either.
//!
//! What the owner signs (all text, UTF-8, `\n` between lines, no trailing newline):
//!
//!   near-flows stealth v1
//!   vault: <this vault's account id>
//!   nonce: <u64, decimal>
//!   expires_ns: <u64, decimal: block time after which it's refused>
//!   tgas: <u64, decimal: gas the command runs with>
//!   method: <vault method>
//!   args: <the method's JSON arguments, verbatim>
//!
//! The vault id is in it, so a command signed for one vault is useless on another
//! (even one with the same key). Nonces only go up: a command runs once, and a
//! newer one makes every older unsent one void. The gas is signed so a relayer
//! can't starve the command into failing (which would still burn its nonce).
//!
//! Honest limits:
//! - Money in and out still links. Whoever funds the vault and wherever a
//!   withdrawal lands are public. `withdraw_to` sends to any account the owner
//!   signs for, or into NEAR Intents credited to any account; use a fresh one, and
//!   remember amounts and timing can still be matched against the owner's wallet.
//! - Timing: commands land when the owner signs them, through one relayer; a
//!   relayer that also knows the owner (e.g. their app's backend) can link them.
//! - The relayer pays gas. A fee for it can be charged here later (see `exec`).

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, require, AccountId, CurveType, Gas, GasWeight, NearToken, Promise, PublicKey};

use crate::{Vault, VaultExt, NEAR};

/// Stored apart from the main state, so the vault's struct (edited by many) doesn't change.
const KEY: &[u8] = b"stealth";
/// What `exec` itself needs on top of the command's gas: checks plus the self-call.
const EXEC_OVERHEAD_TGAS: u64 = 10;

#[near(serializers = [borsh])]
struct Stealth {
    /// The owner's Ed25519 public key.
    key: [u8; 32],
    /// The last nonce used.
    nonce: u64,
}

/// The exact bytes the owner signs. Pinned against the keeper's TypeScript (stealth.ts).
pub fn message(vault: &str, nonce: u64, expires_ns: u64, tgas: u64, method: &str, args: &str) -> String {
    format!("near-flows stealth v1\nvault: {vault}\nnonce: {nonce}\nexpires_ns: {expires_ns}\ntgas: {tgas}\nmethod: {method}\nargs: {args}")
}

fn ed25519(key: &PublicKey) -> [u8; 32] {
    require!(key.curve_type() == CurveType::ED25519, "the owner key must be ed25519");
    key.as_bytes()[1..].try_into().unwrap()
}

fn load() -> Option<Stealth> {
    env::storage_read(KEY).map(|b| near_sdk::borsh::from_slice(&b).unwrap())
}

fn save(s: &Stealth) {
    env::storage_write(KEY, &near_sdk::borsh::to_vec(s).unwrap());
}

#[near]
impl Vault {
    /// A vault owned by a key (`ed25519:<base58>`). It is its own owner; only commands
    /// signed with the key (sent through `exec` by anyone) act as the owner.
    #[init]
    pub fn new_stealth(owner_key: PublicKey, keeper: Option<AccountId>) -> Self {
        save(&Stealth { key: ed25519(&owner_key), nonce: 0 });
        Self::new(env::current_account_id(), keeper)
    }

    /// Run a command the owner signed (see the module doc for the signed bytes).
    /// `nonce`, `expires_ns`: u64 as decimal strings (JSON numbers lose precision past 2^53).
    /// `signature`: 64 bytes, hex. Anyone may send it; the sender pays the gas.
    pub fn exec(&mut self, method: String, args: String, nonce: U64, expires_ns: U64, tgas: u64, signature: String) -> Promise {
        let (nonce, expires_ns) = (nonce.0, expires_ns.0);
        let mut s = load().unwrap_or_else(|| env::panic_str("not a stealth vault"));
        let sig: [u8; 64] = hex::decode(&signature).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("signature: 64 bytes, hex"));
        let me = env::current_account_id();
        require!(env::ed25519_verify(&sig, message(me.as_str(), nonce, expires_ns, tgas, &method, &args), &s.key), "bad signature");
        require!(env::block_timestamp() <= expires_ns, "expired");
        require!(nonce > s.nonce, "nonce already used");
        // Callbacks trust their promise results; a command must never fake one.
        require!(!method.starts_with("on_") && !["new", "new_stealth", "exec"].contains(&method.as_str()), "not a command");
        require!(!method.is_empty() && method.len() <= 64 && !method.contains('\n'), "bad method");
        require!(env::prepaid_gas().as_tgas() >= tgas + EXEC_OVERHEAD_TGAS, format!("attach at least {} Tgas", tgas + EXEC_OVERHEAD_TGAS));
        // Burned now, whatever the command does next: a failed command can't be replayed later.
        s.nonce = nonce;
        save(&s);
        // Later: charge the relayer's fee here (e.g. a fixed yoctoNEAR to the predecessor).
        Promise::new(me).function_call_weight(method, args.into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(tgas), GasWeight(0))
    }

    /// Owner: switch to a new key (e.g. the old one may have leaked). Takes effect at once.
    pub fn set_owner_key(&mut self, owner_key: PublicKey) {
        self.assert_owner();
        let mut s = load().unwrap_or_else(|| env::panic_str("not a stealth vault"));
        s.key = ed25519(&owner_key);
        save(&s);
    }

    /// Owner: send free balance to `to` ("near" for native NEAR). With `via_intents`, the
    /// tokens go into NEAR Intents (the vault's verifier), credited to `to` there, so the
    /// exit is an Intents balance rather than a transfer straight to a wallet.
    /// Either way the receiver is public: pick one that doesn't lead back to you.
    pub fn withdraw_to(&mut self, token: String, amount: U128, to: AccountId, via_intents: bool) -> Promise {
        self.assert_owner();
        self.assert_idle();
        require!(to != env::current_account_id(), "to: another account");
        if token == NEAR {
            require!(!via_intents, "wrap NEAR first: Intents takes tokens");
            require!(amount.0 <= self.free_near(), "not enough free NEAR");
            return Promise::new(to).transfer(NearToken::from_yoctonear(amount.0));
        }
        let t: AccountId = token.parse().unwrap_or_else(|_| env::panic_str("bad token"));
        ext_ft_core::ext(t.clone())
            .with_static_gas(Gas::from_tgas(10))
            .ft_balance_of(env::current_account_id())
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(60)).on_withdraw_to_balance(t, amount, to, via_intents))
    }

    #[private]
    pub fn on_withdraw_to_balance(&mut self, token: AccountId, amount: U128, to: AccountId, via_intents: bool, #[callback_unwrap] balance: U128) -> Promise {
        self.assert_idle();
        let free = balance.0.saturating_sub(self.allocated(token.as_str()));
        require!(amount.0 <= free, "not enough free balance");
        let ft = ext_ft_core::ext(token).with_attached_deposit(NearToken::from_yoctonear(1));
        if via_intents {
            let verifier = self.intents_verifier().unwrap_or_else(|| env::panic_str("run intents_setup first"));
            // The verifier credits the account named in `msg`, not the sender.
            return ft.with_static_gas(Gas::from_tgas(40)).ft_transfer_call(verifier, amount, None, to.to_string());
        }
        ft.with_static_gas(Gas::from_tgas(10)).ft_transfer(to, amount, None)
    }

    /// The owner's key (`ed25519:<base58>`), if this is a stealth vault.
    pub fn get_owner_key(&self) -> Option<String> {
        load().map(|s| format!("ed25519:{}", near_sdk::bs58::encode(s.key).into_string()))
    }

    /// The last nonce used by `exec` (the next command needs a bigger one).
    pub fn get_nonce(&self) -> U64 {
        U64(load().map_or(0, |s| s.nonce))
    }
}

impl Vault {
    #[cfg(feature = "intents")]
    fn intents_verifier(&self) -> Option<AccountId> {
        self.intents.as_ref().map(|c| c.verifier.clone())
    }
    #[cfg(not(feature = "intents"))]
    fn intents_verifier(&self) -> Option<AccountId> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The same vector is pinned in scripts/keeper/src/stealth.test.ts, where the keeper
    // signed it (node:crypto). Here the vault builds the text and checks that signature.
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
        let other = message("other.vaults.near", 1762400000000000000, 1762400600000000000, 50, "withdraw_to", ARGS);
        assert!(!env::ed25519_verify(&sig, &other, &key), "bound to the vault");
    }
}
