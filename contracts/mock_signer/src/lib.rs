//! Test-only stand-in for the MPC signer (v1.signer): same method names and
//! JSON shapes, but one known Ed25519 key per caller (derived from its account
//! id), so tests can check the signatures. NEVER for real use: the "secret" is
//! public.

use ed25519_dalek::{Signer, SigningKey};
use near_sdk::serde_json::{json, Value};
use near_sdk::{env, near, require, AccountId};

#[near(contract_state)]
#[derive(Default)]
pub struct MockSigner {}

fn key_for(predecessor: &str, path: &str) -> SigningKey {
    let seed = env::sha256_array(format!("mock-signer|{predecessor}|{path}").as_bytes());
    SigningKey::from_bytes(&seed)
}

#[near(serializers = [json])]
pub struct Request {
    path: String,
    payload_v2: Value,
    domain_id: u32,
}

#[near]
impl MockSigner {
    pub fn derived_public_key(&self, path: String, predecessor: AccountId, domain_id: u32) -> String {
        require!(domain_id == 1, "mock: ed25519 only");
        let pk = key_for(predecessor.as_str(), &path).verifying_key();
        format!("ed25519:{}", near_sdk::bs58::encode(pk.as_bytes()).into_string())
    }

    #[payable]
    pub fn sign(&mut self, request: Request) -> Value {
        require!(env::attached_deposit().as_yoctonear() >= 1, "attach a deposit");
        require!(request.domain_id == 1, "mock: ed25519 only");
        let hex_payload = request.payload_v2.get("Eddsa").and_then(Value::as_str).unwrap_or_else(|| env::panic_str("Payload is not EdDSA"));
        let msg = hex::decode(hex_payload).unwrap_or_else(|_| env::panic_str("bad hex"));
        let sig = key_for(env::predecessor_account_id().as_str(), &request.path).sign(&msg);
        json!({"scheme": "Ed25519", "signature": sig.to_bytes().to_vec()})
    }
}
