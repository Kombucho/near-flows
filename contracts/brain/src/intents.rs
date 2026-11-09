//! NEAR Intents. The vault's MPC-derived Ed25519 key
//! (path "intents-1", derived from the VAULT's account) is registered on the
//! vault's own Intents account; only the vault can ask the MPC network to sign
//! with it, and the brain only ever has it sign intents built here (intentsig).
//!
//! What moved: the reads. The brain asks the signer for the vault's derived
//! key (a view taking the predecessor as an argument) and the verifier for its
//! salt itself. What stays with the vault: `add_public_key`, `sign` and
//! `ft_withdraw`, which must come from the vault's account.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use crate::wire::Answer;
use crate::{mul_div, result, Next, Read, Reply, Talk, GAS_READ};

pub const INTENTS_PATH: &str = "intents-1";
const EDDSA_DOMAIN: u32 = 1;
const GAS_SIGN: Gas = Gas::from_tgas(50);
/// on_intents_signed: burns 2.8 Tgas; 8 is a ~3x margin.
pub(crate) const GAS_SIGNED: Gas = Gas::from_tgas(8);
const MAX_VALID_SECS: u64 = 3_600;

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct IntentsPair {
    pub token_in: AccountId,
    pub token_out: AccountId,
    pub max_in: U128,
    pub floor_out: U128,
    pub floor_per_in: U128,
    /// A floor that follows the market, on top of the hard floor (oracle.rs).
    #[serde(default)]
    pub market: Option<crate::Market>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct IntentsConfig {
    pub verifier: AccountId,
    pub signer: AccountId,
    pub key: Option<String>,
    pub pairs: Vec<IntentsPair>,
    /// Tokens that may leave for the vault's own address on another chain (hop.rs).
    #[serde(default)]
    pub hops: Vec<crate::hop::Hop>,
}

/// A swap being signed, carried through the reads.
#[near(serializers = [borsh])]
pub struct Pending {
    token_in: AccountId,
    amount_in: u128,
    token_out: AccountId,
    min_out: u128,
    deadline_ns: u64,
}

impl Talk {
    pub(crate) fn intents_setup(&mut self, verifier: AccountId, signer: AccountId) -> Reply {
        self.vs.intents = Some(IntentsConfig { verifier, signer: signer.clone(), key: None, pairs: vec![], hops: vec![] });
        let args = near_sdk::serde_json::json!({"path": INTENTS_PATH, "predecessor": self.vault, "domain_id": EDDSA_DOMAIN}).to_string();
        self.read(vec![(signer, "derived_public_key".into(), args)], Read::IntentsKey, GAS_READ)
    }

    pub(crate) fn on_intents_key(&mut self) -> Reply {
        let raw = result(0, 256).unwrap_or_else(|| env::panic_str("the signer didn't answer"));
        let key: String = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("bad key"));
        let b58 = key.strip_prefix("ed25519:").unwrap_or_else(|| env::panic_str("expected an ed25519 key")).to_string();
        let cfg = self.vs.intents.as_mut().unwrap();
        cfg.key = Some(b58.clone());
        let verifier = cfg.verifier.clone();
        self.emit("intents_key", &format!(r#""public_key":"ed25519:{b58}""#));
        Answer::default().call(&verifier, "add_public_key", format!(r#"{{"public_key":"ed25519:{b58}"}}"#).as_bytes(), 1, Gas::from_tgas(10)).into()
    }

    pub(crate) fn intents_allow(&mut self, pair: IntentsPair) {
        require!(pair.floor_per_in.0 > 0 && pair.floor_out.0 > 0, "set a floor");
        let cfg = self.vs.intents.as_mut().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        cfg.pairs.retain(|p| !(p.token_in == pair.token_in && p.token_out == pair.token_out));
        if pair.max_in.0 > 0 {
            require!(cfg.pairs.len() < 16, "too many pairs");
            cfg.pairs.push(pair);
        }
    }

    pub(crate) fn intents_sign_swap(&mut self, _by_owner: bool, token_in: AccountId, token_out: AccountId, amount_in: u128, min_out: u128, valid_secs: u64) -> Reply {
        let cfg = self.vs.intents.clone().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        require!(cfg.key.is_some(), "the vault's Intents key isn't known yet");
        let pair = cfg.pairs.iter().find(|p| p.token_in == token_in && p.token_out == token_out).cloned()
            .unwrap_or_else(|| env::panic_str("refused: pair not allowed"));
        require!(amount_in > 0 && amount_in <= pair.max_in.0, "refused: amount over the pair's limit");
        require!((60..=MAX_VALID_SECS).contains(&valid_secs), "valid for 1 to 60 minutes");
        let need = GAS_SIGN.as_tgas() + GAS_SIGNED.as_tgas() + crate::RELAY_TGAS;
        self.need_after = need;
        require!(self.ctx.gas_left >= Gas::from_tgas(need).as_gas(), "attach more gas to sign an intent");
        let floor = mul_div(amount_in, pair.floor_out.0, pair.floor_per_in.0);
        let out_min = min_out.max(floor).max(1);
        self.charge(&format!("intents:{token_in}"), amount_in);
        let pending = Pending { token_in, amount_in, token_out, min_out: out_min, deadline_ns: env::block_timestamp() + valid_secs * 1_000_000_000 };
        // The salt comes from the verifier itself, and a market floor's prices from
        // the oracle: all read by the brain, in one round.
        let mut views = vec![(cfg.verifier, "current_salt".into(), "{}".into())];
        if let Some(m) = &pair.market {
            let feeds = [m.sell.clone(), m.buy.clone()];
            self.require_confirmed(&feeds);
            views.extend(self.feed_views(&feeds));
        }
        self.read(views, Read::IntentsSalt(pending), GAS_READ)
    }

    pub(crate) fn on_intents_salt(&mut self, mut p: Pending) -> Reply {
        let raw = result(0, 256).unwrap_or_else(|| env::panic_str("the verifier didn't answer"));
        let salt: String = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("bad salt"));
        let salt: [u8; 4] = hex::decode(&salt).ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str("bad salt"));
        let cfg = self.vs.intents.clone().unwrap();
        let market = cfg.pairs.iter().find(|x| x.token_in == p.token_in && x.token_out == p.token_out).and_then(|x| x.market.clone());
        if let Some(m) = market {
            let pr = self.prices(&[m.sell.clone(), m.buy.clone()], 1);
            p.min_out = p.min_out.max(crate::oracle::market_min(&m, p.amount_in, pr[0], pr[1]));
        }
        let random: [u8; 15] = env::random_seed_array()[..15].try_into().unwrap();
        let nonce = intentsig::versioned_nonce(salt, p.deadline_ns as i64, random);
        // The signer of the intent is the VAULT: its own Intents account, its own key.
        let message = intentsig::swap_message(self.vault.as_str(), p.deadline_ns, p.token_in.as_str(), p.amount_in, p.token_out.as_str(), p.min_out);
        let hash = intentsig::nep413_hash(&message, &nonce, cfg.verifier.as_str());
        self.emit("intents_sign_requested", &format!(r#""token_in":"{}","amount_in":"{}","token_out":"{}","min_out":"{}""#, p.token_in, p.amount_in, p.token_out, p.min_out));
        let args = near_sdk::serde_json::json!({"request": {"path": INTENTS_PATH, "payload_v2": {"Eddsa": hex::encode(hash)}, "domain_id": EDDSA_DOMAIN}}).to_string();
        let next = near_sdk::borsh::to_vec(&Next::IntentsSigned { message, nonce }).unwrap();
        self.charge_signature(Answer::default().call(&cfg.signer, "sign", args.as_bytes(), 1, GAS_SIGN)).then("on_vault", next, GAS_SIGNED).into()
    }

    /// The vault's `sign` came back: the signed intent goes out as an event and
    /// as the vault's return value (for the keeper to hand a solver).
    pub(crate) fn on_intents_signed(&mut self, message: String, nonce: [u8; 32]) -> Reply {
        let cfg = self.vs.intents.clone().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        let Some(sig) = result(0, 4096).and_then(|raw| signature_from(&raw)) else {
            self.emit("intents_sign_failed", "");
            return Answer::default().into();
        };
        let signed = intentsig::signed_json(&message, &nonce, cfg.verifier.as_str(), cfg.key.as_deref().unwrap(), &near_sdk::bs58::encode(sig).into_string());
        self.emit("intents_signed", &format!(r#""signed":{signed}"#));
        Answer::default().ret(&signed).into()
    }

    /// Bring tokens back from the vault's Intents account: always to the vault itself.
    /// Never during a step: tokens landing between its two balance reads would
    /// count as the step's output.
    pub(crate) fn intents_withdraw(&mut self, token: AccountId, amount: u128) -> Answer {
        self.idle();
        let cfg = self.vs.intents.clone().unwrap_or_else(|| env::panic_str("run intents_setup first"));
        let args = near_sdk::serde_json::json!({"token": token, "receiver_id": self.vault, "amount": amount.to_string()}).to_string();
        Answer::default().call(&cfg.verifier, "ft_withdraw", args.as_bytes(), 1, Gas::from_tgas(60))
    }
}

/// The 64-byte Ed25519 signature inside the signer's answer, whichever JSON shape it uses.
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
