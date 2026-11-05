//! NEAR Intents from the vault, signed through Chain Signatures.
//!
//! The vault registers its MPC-derived Ed25519 key (path "intents-1") on its own
//! Intents account. Only the vault can ask the MPC network to sign with that key,
//! so the only intents ever signed are the ones built here (intentsig):
//! "give exactly X of A, receive at least Y of B, before the deadline", where Y is
//! at least the owner's floor. A solver matches it and anyone submits both to
//! `execute_intents`. Withdrawals from Intents only ever go to the vault itself.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise};

use crate::{Vault, VaultExt};

pub const INTENTS_PATH: &str = "intents-1";
const EDDSA_DOMAIN: u32 = 1;
const GAS_SIGN: Gas = Gas::from_tgas(50);
const GAS_CB: Gas = Gas::from_tgas(15);
const ONE_YOCTO: NearToken = NearToken::from_yoctonear(1);
/// An intent is valid for at most this long.
const MAX_VALID_SECS: u64 = 3_600;

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct IntentsPair {
    /// NEP-141 contracts
    pub token_in: AccountId,
    pub token_out: AccountId,
    pub max_in: U128,
    /// least output per input, scaled 1e12
    pub min_out_per_in_e12: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct IntentsConfig {
    /// The Intents verifier (intents.near, or a sandbox running its code).
    pub verifier: AccountId,
    pub signer: AccountId,
    /// The vault's Ed25519 key on Intents (base58), from the signer.
    pub key: Option<String>,
    pub pairs: Vec<IntentsPair>,
}

/// A swap being signed, carried through the callbacks.
#[near(serializers = [json])]
pub struct Pending {
    token_in: AccountId,
    amount_in: U128,
    token_out: AccountId,
    min_out: U128,
    deadline_ns: u64,
}

#[near]
impl Vault {
    /// Owner: use an Intents verifier; the vault learns its Intents key from the signer
    /// and registers it on its own Intents account.
    pub fn intents_setup(&mut self, verifier: AccountId, signer: AccountId) -> Promise {
        self.assert_owner();
        self.intents = Some(IntentsConfig { verifier, signer: signer.clone(), key: None, pairs: vec![] });
        let args = near_sdk::serde_json::json!({"path": INTENTS_PATH, "predecessor": env::current_account_id(), "domain_id": EDDSA_DOMAIN});
        Promise::new(signer)
            .function_call("derived_public_key".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(30)).on_intents_key())
    }

    #[private]
    pub fn on_intents_key(&mut self, #[callback_unwrap] key: String) -> Promise {
        let b58 = key.strip_prefix("ed25519:").unwrap_or_else(|| env::panic_str("expected an ed25519 key")).to_string();
        let cfg = self.intents.as_mut().unwrap();
        cfg.key = Some(b58.clone());
        let verifier = cfg.verifier.clone();
        self.emit("intents_key", &format!(r#"{{"public_key":"ed25519:{b58}"}}"#));
        Promise::new(verifier).function_call(
            "add_public_key".to_string(),
            format!(r#"{{"public_key":"ed25519:{b58}"}}"#).into_bytes(),
            ONE_YOCTO,
            Gas::from_tgas(10),
        )
    }

    /// Owner: allow a pair, or remove it with `max_in: "0"`.
    pub fn intents_allow(&mut self, pair: IntentsPair) {
        self.assert_owner();
        let cfg = self.intents.as_mut().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        cfg.pairs.retain(|p| !(p.token_in == pair.token_in && p.token_out == pair.token_out));
        if pair.max_in.0 > 0 {
            require!(cfg.pairs.len() < 16, "too many pairs");
            cfg.pairs.push(pair);
        }
    }

    /// Owner or keeper: sign "give `amount_in` of `token_in`, get at least max(`min_out`, floor)
    /// of `token_out`", valid for `valid_secs`. The signed intent is emitted as `intents_signed`
    /// for a solver to match and submit.
    pub fn intents_sign_swap(&mut self, token_in: AccountId, token_out: AccountId, amount_in: U128, min_out: U128, valid_secs: u64) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        let cfg = self.intents.clone().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        require!(cfg.key.is_some(), "the vault's Intents key isn't known yet");
        let pair = cfg.pairs.iter().find(|p| p.token_in == token_in && p.token_out == token_out).cloned()
            .unwrap_or_else(|| env::panic_str("refused: pair not allowed"));
        require!(amount_in.0 > 0 && amount_in.0 <= pair.max_in.0, "refused: amount over the pair's limit");
        require!((60..=MAX_VALID_SECS).contains(&valid_secs), "valid for 1 to 60 minutes");
        let floor = crate::mul_div(amount_in.0, pair.min_out_per_in_e12.0, 1_000_000_000_000);
        let out_min = min_out.0.max(floor).max(1);
        self.charge(&format!("intents:{token_in}"), amount_in.0);
        let pending = Pending { token_in, amount_in, token_out, min_out: U128(out_min), deadline_ns: env::block_timestamp() + valid_secs * 1_000_000_000 };
        // The salt comes from the verifier itself, not from anyone's say-so.
        Promise::new(cfg.verifier)
            .function_call("current_salt".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), Gas::from_tgas(5))
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(80)).on_intents_salt(pending))
    }

    #[private]
    pub fn on_intents_salt(&mut self, pending: Pending, #[callback_unwrap] salt: String) -> Promise {
        let salt: [u8; 4] = hex::decode(&salt).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("bad salt"));
        let cfg = self.intents.clone().unwrap();
        let random: [u8; 15] = env::random_seed_array()[..15].try_into().unwrap();
        let nonce = intentsig::versioned_nonce(salt, pending.deadline_ns as i64, random);
        let me = env::current_account_id();
        let message = intentsig::swap_message(me.as_str(), pending.deadline_ns, pending.token_in.as_str(), pending.amount_in.0, pending.token_out.as_str(), pending.min_out.0);
        let hash = intentsig::nep413_hash(&message, &nonce, cfg.verifier.as_str());
        self.emit("intents_sign_requested", &format!(r#"{{"token_in":"{}","amount_in":"{}","token_out":"{}","min_out":"{}"}}"#,
            pending.token_in, pending.amount_in.0, pending.token_out, pending.min_out.0));
        let args = near_sdk::serde_json::json!({"request": {"path": INTENTS_PATH, "payload_v2": {"Eddsa": hex::encode(hash)}, "domain_id": EDDSA_DOMAIN}});
        Promise::new(cfg.signer)
            .function_call("sign".to_string(), args.to_string().into_bytes(), ONE_YOCTO, GAS_SIGN)
            .then(Self::ext(me).with_static_gas(GAS_CB).on_intents_signed(message, hex::encode(nonce)))
    }

    #[private]
    pub fn on_intents_signed(&mut self, message: String, nonce: String) {
        let cfg = self.intents.clone().unwrap();
        let sig = env::promise_result_checked(0, 4096).ok().and_then(|raw| crate::sol::signature_from(&raw));
        let Some(sig) = sig else {
            self.emit("intents_sign_failed", "{}");
            return;
        };
        let nonce: [u8; 32] = hex::decode(nonce).unwrap().try_into().unwrap();
        let signed = intentsig::signed_json(&message, &nonce, cfg.verifier.as_str(), cfg.key.as_deref().unwrap(), &near_sdk::bs58::encode(sig).into_string());
        self.emit("intents_signed", &format!(r#"{{"signed":{signed}}}"#));
    }

    /// Owner or keeper: bring tokens back from the vault's Intents account. Always to the vault itself.
    pub fn intents_withdraw(&mut self, token: AccountId, amount: U128) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        let cfg = self.intents.clone().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        let args = near_sdk::serde_json::json!({"token": token, "receiver_id": env::current_account_id(), "amount": amount});
        Promise::new(cfg.verifier).function_call("ft_withdraw".to_string(), args.to_string().into_bytes(), ONE_YOCTO, Gas::from_tgas(60))
    }

    pub fn get_intents(&self) -> Option<IntentsConfig> {
        self.intents.clone()
    }
}
