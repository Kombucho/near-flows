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

fn k256_for(predecessor: &str, path: &str) -> k256::ecdsa::SigningKey {
    let seed = env::sha256_array(format!("mock-signer-k256|{predecessor}|{path}").as_bytes());
    k256::ecdsa::SigningKey::from_bytes((&seed).into()).unwrap()
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
        if domain_id == 0 {
            let pk = k256_for(predecessor.as_str(), &path).verifying_key().to_encoded_point(false);
            return format!("secp256k1:{}", near_sdk::bs58::encode(&pk.as_bytes()[1..]).into_string());
        }
        require!(domain_id == 1, "mock: domains 0 and 1");
        let pk = key_for(predecessor.as_str(), &path).verifying_key();
        format!("ed25519:{}", near_sdk::bs58::encode(pk.as_bytes()).into_string())
    }

    #[payable]
    pub fn sign(&mut self, request: Request) -> Value {
        require!(env::attached_deposit().as_yoctonear() >= 1, "attach a deposit");
        if request.domain_id == 0 {
            let h = request.payload_v2.get("Ecdsa").and_then(Value::as_str).unwrap_or_else(|| env::panic_str("Payload is not ECDSA"));
            let hash = hex::decode(h).unwrap_or_else(|_| env::panic_str("bad hex"));
            let key = k256_for(env::predecessor_account_id().as_str(), &request.path);
            let (sig, rec) = key.sign_prehash_recoverable(&hash).unwrap_or_else(|_| env::panic_str("sign failed"));
            let (r, mut s) = (sig.r().to_bytes(), sig.s().to_bytes());
            let mut parity = rec.is_y_odd() as u8;
            // Like the real network, don't promise a low s: flip half of them to high s (n - s),
            // so the vault's EIP-2 normalisation is exercised.
            if hash[0] & 1 == 1 {
                let high = -*sig.s();
                s = high.to_bytes();
                parity ^= 1;
            }
            let mut big_r = vec![2 + parity];
            big_r.extend_from_slice(&r);
            return json!({"scheme": "Secp256k1", "big_r": {"affine_point": hex::encode(big_r)}, "s": {"scalar": hex::encode(s)}, "recovery_id": parity});
        }
        require!(request.domain_id == 1, "mock: domains 0 and 1");
        let hex_payload = request.payload_v2.get("Eddsa").and_then(Value::as_str).unwrap_or_else(|| env::panic_str("Payload is not EdDSA"));
        let msg = hex::decode(hex_payload).unwrap_or_else(|_| env::panic_str("bad hex"));
        let sig = key_for(env::predecessor_account_id().as_str(), &request.path).sign(&msg);
        json!({"scheme": "Ed25519", "signature": sig.to_bytes().to_vec()})
    }
}
