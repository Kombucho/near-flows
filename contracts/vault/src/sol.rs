//! Solana from the vault, through NEAR Chain Signatures.
//!
//! The vault has a Solana address derived from its own NEAR account by the MPC
//! signer (`derived_public_key`), and only the vault can ask for signatures
//! under it. A helper (keeper) fetches a Jupiter route and passes the message
//! here; the vault checks every byte against the owner's policy (solguard),
//! counts the amount it read from the message against its spending limit, and
//! only then asks the signer. The signature comes back in an event; the helper
//! broadcasts it. The helper can't get anything else signed.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise};

use crate::{Vault, VaultExt};

pub const SOL_PATH: &str = "solana-1";
const EDDSA_DOMAIN: u32 = 1;
const GAS_SIGN: Gas = Gas::from_tgas(50);
const GAS_VIEW: Gas = Gas::from_tgas(10);
const GAS_CB: Gas = Gas::from_tgas(10);

/// One swap pair the owner allows on Solana.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolPair {
    /// base58 mint addresses
    pub in_mint: String,
    pub out_mint: String,
    /// most input per swap, in the input token's base units
    pub max_in: U128,
    /// least output per input, scaled 1e12
    pub min_out_per_in_e12: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolConfig {
    /// The MPC signer contract (v1.signer-prod.testnet on testnet).
    pub signer: AccountId,
    /// The vault's Solana address (base58), read from the signer itself.
    pub address: Option<String>,
    pub pairs: Vec<SolPair>,
    pub max_cu_price: u64,
    pub max_cu_limit: u32,
}

#[near]
impl Vault {
    /// Owner: point the vault at an MPC signer and learn its Solana address from it.
    pub fn sol_setup(&mut self, signer: AccountId, max_cu_price: u64, max_cu_limit: u32) -> Promise {
        self.assert_owner();
        require!(max_cu_limit <= 1_400_000, "compute limit: at most 1.4M");
        self.sol = Some(SolConfig { signer: signer.clone(), address: None, pairs: vec![], max_cu_price, max_cu_limit });
        let args = near_sdk::serde_json::json!({"path": SOL_PATH, "predecessor": env::current_account_id(), "domain_id": EDDSA_DOMAIN});
        Promise::new(signer)
            .function_call("derived_public_key".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(0), GAS_VIEW)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CB).on_sol_address())
    }

    #[private]
    pub fn on_sol_address(&mut self, #[callback_unwrap] key: String) {
        let b58 = key.strip_prefix("ed25519:").unwrap_or_else(|| env::panic_str("expected an ed25519 key"));
        let bytes = near_sdk::bs58::decode(b58).into_vec().unwrap_or_else(|_| env::panic_str("bad key"));
        require!(bytes.len() == 32, "bad key length");
        let cfg = self.sol.as_mut().unwrap_or_else(|| env::panic_str("not set up"));
        cfg.address = Some(b58.to_string());
        self.emit("sol_address", &format!(r#"{{"address":"{b58}"}}"#));
    }

    /// Owner: allow (or replace) a swap pair, or remove it with `max_in: "0"`.
    pub fn sol_allow(&mut self, pair: SolPair) {
        self.assert_owner();
        for m in [&pair.in_mint, &pair.out_mint] {
            require!(near_sdk::bs58::decode(m).into_vec().map_or(false, |v| v.len() == 32), "mints are base58 addresses");
        }
        let cfg = self.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.pairs.retain(|p| !(p.in_mint == pair.in_mint && p.out_mint == pair.out_mint));
        if pair.max_in.0 > 0 {
            require!(cfg.pairs.len() < 16, "too many pairs");
            cfg.pairs.push(pair);
        }
    }

    /// Owner or keeper: have a Jupiter swap message signed, if the policy allows it.
    /// `message`: the Solana message bytes, base64. The signature is emitted as
    /// event `sol_signed` (message hash + signature hex) for the helper to broadcast.
    pub fn sol_sign_swap(&mut self, message: String) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        let bytes = near_sdk::base64::Engine::decode(&near_sdk::base64::engine::general_purpose::STANDARD, &message)
            .unwrap_or_else(|_| env::panic_str("message: base64"));
        require!(bytes.len() <= 1232, "a Solana message fits in 1232 bytes");
        let cfg = self.sol.clone().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        let address = cfg.address.clone().unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known yet"));
        let authority = solguard::pubkey(&address);
        // The first allowed pair whose policy accepts the message.
        let mut why = String::from("no pair allowed");
        let mut found = None;
        for pair in &cfg.pairs {
            let policy = solguard::SwapPolicy {
                authority,
                in_mint: solguard::pubkey(&pair.in_mint),
                out_mint: solguard::pubkey(&pair.out_mint),
                max_in: u64::try_from(pair.max_in.0).unwrap_or(u64::MAX),
                min_out_per_in_e12: pair.min_out_per_in_e12.0,
                max_cu_price: cfg.max_cu_price,
                max_cu_limit: cfg.max_cu_limit,
            };
            match solguard::check_swap(&bytes, &policy) {
                Ok(v) => {
                    found = Some((pair.clone(), v));
                    break;
                }
                Err(e) => why = format!("{e:?}"),
            }
        }
        let (pair, verdict) = found.unwrap_or_else(|| env::panic_str(&format!("refused: {why}")));
        // The amount counted is the one READ FROM THE MESSAGE, not what anyone claims.
        self.charge(&format!("sol:{}", pair.in_mint), verdict.in_amount as u128);
        let hash = hex::encode(env::sha256(&bytes));
        let args = near_sdk::serde_json::json!({"request": {"path": SOL_PATH, "payload_v2": {"Eddsa": hex::encode(&bytes)}, "domain_id": EDDSA_DOMAIN}});
        self.emit(
            "sol_sign_requested",
            &format!(r#"{{"hash":"{hash}","in_mint":"{}","out_mint":"{}","in_amount":"{}","min_out":"{}","max_priority_fee_lamports":"{}"}}"#,
                pair.in_mint, pair.out_mint, verdict.in_amount, verdict.min_out, verdict.max_priority_fee_lamports),
        );
        Promise::new(cfg.signer)
            .function_call("sign".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(1), GAS_SIGN)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CB).on_sol_signed(hash))
    }

    /// The signer answered: publish the signature for the helper to broadcast.
    #[private]
    pub fn on_sol_signed(&mut self, hash: String) {
        let raw = env::promise_result_checked(0, 4096).ok();
        let sig = raw.as_deref().and_then(signature_from).map(hex::encode);
        match sig {
            Some(s) => self.emit("sol_signed", &format!(r#"{{"hash":"{hash}","signature":"{s}"}}"#)),
            None => self.emit("sol_sign_failed", &format!(r#"{{"hash":"{hash}"}}"#)),
        }
    }

    pub fn get_sol(&self) -> Option<SolConfig> {
        self.sol.clone()
    }
}

/// The 64-byte Ed25519 signature inside the signer's answer, whichever JSON
/// shape it uses: {"scheme":"Ed25519","signature":[..64]} or {"signature":{"bytes":[..]}}
/// or a hex string.
pub(crate) fn signature_from(raw: &[u8]) -> Option<Vec<u8>> {
    use near_sdk::serde_json::Value;
    fn find(v: &Value) -> Option<Vec<u8>> {
        match v {
            Value::Array(a) if a.len() == 64 => a.iter().map(|x| x.as_u64().map(|n| n as u8)).collect(),
            Value::String(s) if s.len() == 128 => hex::decode(s).ok(),
            Value::Object(o) => o.get("signature").and_then(find).or_else(|| o.get("bytes").and_then(find)).or_else(|| o.values().find_map(find)),
            _ => None,
        }
    }
    find(&near_sdk::serde_json::from_slice(raw).ok()?)
}
