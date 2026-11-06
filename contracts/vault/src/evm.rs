//! EVM chains from the vault, through NEAR Chain Signatures (secp256k1).
//!
//! The vault BUILDS every transaction it signs, from a TEMPLATE its owner set:
//! a contract, a function signature, and a rule for each argument:
//! - fixed: always this address or number (the pool's tokens, a fee tier),
//! - vault: the vault's own EVM address (recipient, onBehalfOf, to),
//! - at_most: a number the keeper picks, up to a cap (an amount),
//! - floor: a minimum the keeper quotes, never below amount x num / den (the owner's floor).
//! A keeper fills in numbers; it never picks an address. There is no rule for
//! "an address the caller chooses", so no template can pay a stranger.
//! Uniswap swaps, ERC-20 approvals and Aave supply/withdraw are all templates
//! (nodes/evm-templates.json); a new protocol is a new template, not new code.
//!
//! The keeper also supplies nonce and gas prices (capped by the owner). The vault
//! signs the hash, assembles the signed transaction, and emits it for broadcast.

use evmtx::sig::Kind;
use evmtx::{abi, address_of, hex_address, parse_address, Eip1559};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise};

use crate::{Vault, VaultExt};

pub const EVM_PATH: &str = "evm-1";
const ECDSA_DOMAIN: u32 = 0;
const GAS_SIGN: Gas = Gas::from_tgas(50);
const GAS_CB: Gas = Gas::from_tgas(15);
const MAX_TEMPLATES: usize = 32;
/// secp256k1 group order n, big-endian (for the low-s rule, EIP-2).
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e,
    0x8c, 0xd0, 0x36, 0x41, 0x41,
];

/// The rule for one argument of a template.
#[near(serializers = [borsh, json])]
#[serde(tag = "rule", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    /// Always this value: a 0x address, or a number (decimal).
    Fixed { value: String },
    /// The vault's own EVM address.
    Vault,
    /// A number the keeper picks, at most `max`.
    AtMost { max: U128 },
    /// A minimum the keeper quotes; the vault uses at least `args[of] * num / den`.
    Floor { of: u8, num: U128, den: U128 },
}

/// Which argument spends a token, and which token: counted against the spending limit "evm:<chain>:<token>".
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub arg: u8,
    pub token: String,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmTemplate {
    /// The node's name, e.g. "uniswap_swap_weth_usdc".
    pub name: String,
    /// The contract it calls.
    pub to: String,
    /// The function, e.g. "approve(address,uint256)". Static arguments only.
    pub function: String,
    pub args: Vec<Arg>,
    pub spends: Option<Spend>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmConfig {
    pub signer: AccountId,
    pub chain_id: u64,
    /// The vault's EVM address, from the signer.
    pub address: Option<String>,
    pub templates: Vec<EvmTemplate>,
    pub max_fee: U128,
    pub max_tip: U128,
    pub gas_cap: u64,
}

/// A transaction the vault built, carried to the signature callback.
#[near(serializers = [json])]
pub struct TxJson {
    nonce: u64,
    max_priority_fee: U128,
    max_fee: U128,
    gas_limit: u64,
    to: String,
    data: String,
}

#[near]
impl Vault {
    /// Owner: one EVM chain and gas caps. Learns the vault's address from the signer.
    pub fn evm_setup(&mut self, signer: AccountId, chain_id: u64, max_fee: U128, max_tip: U128, gas_cap: u64) -> Promise {
        self.assert_owner();
        require!(gas_cap <= 2_000_000, "gas cap: at most 2M");
        let templates = self.evm.as_ref().filter(|c| c.chain_id == chain_id).map(|c| c.templates.clone()).unwrap_or_default();
        self.evm = Some(EvmConfig { signer: signer.clone(), chain_id, address: None, templates, max_fee, max_tip, gas_cap });
        let args = near_sdk::serde_json::json!({"path": EVM_PATH, "predecessor": env::current_account_id(), "domain_id": ECDSA_DOMAIN});
        Promise::new(signer)
            .function_call("derived_public_key".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(0), Gas::from_tgas(10))
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CB).on_evm_address())
    }

    #[private]
    pub fn on_evm_address(&mut self, #[callback_unwrap] key: String) {
        let b58 = key.strip_prefix("secp256k1:").unwrap_or_else(|| env::panic_str("expected a secp256k1 key"));
        let pk = near_sdk::bs58::decode(b58).into_vec().unwrap_or_else(|_| env::panic_str("bad key"));
        let addr = hex_address(&address_of(&pk).unwrap_or_else(|| env::panic_str("bad key length")));
        self.evm.as_mut().unwrap().address = Some(addr.clone());
        self.emit("evm_address", &format!(r#"{{"address":"{addr}"}}"#));
    }

    /// Owner: add a template, or replace the one with the same name.
    pub fn evm_template(&mut self, template: EvmTemplate) {
        self.assert_owner();
        let mut t = template;
        t.to = t.to.to_lowercase();
        check_template(&mut t);
        let cfg = self.evm.as_mut().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        cfg.templates.retain(|x| x.name != t.name);
        require!(cfg.templates.len() < MAX_TEMPLATES, "too many templates");
        cfg.templates.push(t);
    }

    /// Owner: remove a template.
    pub fn evm_template_remove(&mut self, name: String) {
        self.assert_owner();
        if let Some(cfg) = self.evm.as_mut() {
            cfg.templates.retain(|x| x.name != name);
        }
    }

    /// Owner or keeper: call template `name` with `values` (one per argument; only
    /// at_most and floor arguments read theirs). The vault builds, checks and signs it.
    pub fn evm_call(&mut self, name: String, values: Vec<U128>, nonce: u64, max_fee: U128, max_tip: U128, gas: u64) -> Promise {
        let cfg = self.evm_caller_cfg();
        let t = cfg.templates.iter().find(|t| t.name == name).cloned().unwrap_or_else(|| env::panic_str("refused: no such template"));
        require!(values.len() == t.args.len(), "one value per argument");
        let me = parse_address(cfg.address.as_deref().unwrap_or_else(|| env::panic_str("the vault's EVM address isn't known yet"))).unwrap();
        let kinds = evmtx::sig::words(&t.function).unwrap();
        let mut data = abi::selector(&t.function).to_vec();
        for (i, (rule, kind)) in t.args.iter().zip(&kinds).enumerate() {
            let word = match rule {
                Arg::Fixed { value } => fixed_word(value, *kind),
                Arg::Vault => addr_word(&me),
                Arg::AtMost { max } => {
                    require!(values[i].0 <= max.0, format!("refused: argument {i} over its cap"));
                    uint_word(values[i].0, *kind)
                }
                Arg::Floor { of, num, den } => {
                    let floor = crate::mul_div(values[*of as usize].0, num.0, den.0);
                    uint_word(values[i].0.max(floor), *kind)
                }
            };
            data.extend(word);
        }
        if let Some(sp) = &t.spends {
            self.charge(&format!("evm:{}:{}", cfg.chain_id, sp.token), values[sp.arg as usize].0);
        }
        let tx = self.evm_tx(&cfg, nonce, max_fee, max_tip, gas, parse_address(&t.to).unwrap(), data);
        self.emit("evm_call_requested", &format!(r#"{{"template":"{name}","to":"{}","data":"0x{}"}}"#, t.to, hex::encode(&tx.data)));
        self.evm_sign(cfg.signer, tx)
    }

    #[private]
    pub fn on_evm_signed(&mut self, tx: TxJson) {
        let cfg = self.evm.clone().unwrap();
        let t = Eip1559 {
            chain_id: cfg.chain_id, nonce: tx.nonce, max_priority_fee: tx.max_priority_fee.0, max_fee: tx.max_fee.0, gas_limit: tx.gas_limit,
            to: parse_address(&tx.to).unwrap(), value: 0, data: hex::decode(&tx.data).unwrap(),
        };
        let hash = hex::encode(t.signing_hash());
        let sig = env::promise_result_checked(0, 4096).ok().and_then(|raw| ecdsa_from(&raw));
        match sig {
            Some((r, s, v)) => {
                let raw = hex::encode(t.signed(v, &r, &s));
                self.emit("evm_signed", &format!(r#"{{"hash":"0x{hash}","raw":"0x{raw}","r":"{}","s":"{}","v":{v}}}"#, hex::encode(r), hex::encode(s)));
            }
            None => self.emit("evm_sign_failed", &format!(r#"{{"hash":"0x{hash}"}}"#)),
        }
    }

    pub fn get_evm(&self) -> Option<EvmConfig> {
        self.evm.clone()
    }
}

impl Vault {
    fn evm_caller_cfg(&self) -> EvmConfig {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        self.evm.clone().unwrap_or_else(|| env::panic_str("run evm_setup first"))
    }

    #[allow(clippy::too_many_arguments)]
    fn evm_tx(&self, cfg: &EvmConfig, nonce: u64, max_fee: U128, max_tip: U128, gas: u64, to: evmtx::Address, data: Vec<u8>) -> Eip1559 {
        require!(max_fee.0 <= cfg.max_fee.0 && max_tip.0 <= cfg.max_tip.0 && max_tip.0 <= max_fee.0, "refused: gas price over the owner's cap");
        require!(gas <= cfg.gas_cap, "refused: gas over the owner's cap");
        Eip1559 { chain_id: cfg.chain_id, nonce, max_priority_fee: max_tip.0, max_fee: max_fee.0, gas_limit: gas, to, value: 0, data }
    }

    fn evm_sign(&mut self, signer: AccountId, t: Eip1559) -> Promise {
        let args = near_sdk::serde_json::json!({"request": {"path": EVM_PATH, "payload_v2": {"Ecdsa": hex::encode(t.signing_hash())}, "domain_id": ECDSA_DOMAIN}});
        let carry = TxJson { nonce: t.nonce, max_priority_fee: U128(t.max_priority_fee), max_fee: U128(t.max_fee), gas_limit: t.gas_limit, to: hex_address(&t.to), data: hex::encode(&t.data) };
        Promise::new(signer)
            .function_call("sign".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(1), GAS_SIGN)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CB).on_evm_signed(carry))
    }
}

/// (r, s, y_parity) from the signer's answer:
/// {"scheme":"Secp256k1","big_r":{"affine_point":"02|03 + x"},"s":{"scalar":"..."},"recovery_id":0|1},
/// with s made low (EIP-2): if s > n/2 then s = n - s and the parity flips.
fn ecdsa_from(raw: &[u8]) -> Option<([u8; 32], [u8; 32], u8)> {
    let v: near_sdk::serde_json::Value = near_sdk::serde_json::from_slice(raw).ok()?;
    let big_r = hex::decode(v["big_r"]["affine_point"].as_str()?).ok()?;
    let s: [u8; 32] = hex::decode(v["s"]["scalar"].as_str()?).ok()?.try_into().ok()?;
    let rec = v["recovery_id"].as_u64()? as u8;
    if big_r.len() != 33 || rec > 1 {
        return None;
    }
    let r: [u8; 32] = big_r[1..].try_into().ok()?;
    let half = half_n();
    if s > half {
        Some((r, sub(&N, &s), rec ^ 1))
    } else {
        Some((r, s, rec))
    }
}

fn half_n() -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut carry = 0u16;
    for i in 0..32 {
        let cur = (carry << 8) | N[i] as u16;
        out[i] = (cur / 2) as u8;
        carry = cur % 2;
    }
    out
}

fn sub(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let mut borrow = 0i16;
    for i in (0..32).rev() {
        let mut d = a[i] as i16 - b[i] as i16 - borrow;
        borrow = if d < 0 { d += 256; 1 } else { 0 };
        out[i] = d as u8;
    }
    out
}

/// Check a template once, when the owner saves it: a static function, one rule
/// per argument word, each rule fitting its word's type.
fn check_template(t: &mut EvmTemplate) {
    require!(!t.name.is_empty() && t.name.len() <= 40, "name: 1 to 40 characters");
    require!(parse_address(&t.to).is_some(), "to: a 0x address");
    let kinds = evmtx::sig::words(&t.function).unwrap_or_else(|| env::panic_str("function: a signature with static arguments only"));
    require!(kinds.len() == t.args.len() && kinds.len() <= 16, "one rule per argument (at most 16)");
    for (i, (rule, kind)) in t.args.iter_mut().zip(&kinds).enumerate() {
        match rule {
            Arg::Fixed { value } => {
                *value = value.to_lowercase();
                let _ = fixed_word(value, *kind);
            }
            Arg::Vault => require!(*kind == Kind::Address, format!("argument {i}: only an address can be the vault")),
            Arg::AtMost { .. } => require!(matches!(kind, Kind::Uint(_)), format!("argument {i}: at_most needs a number")),
            Arg::Floor { den, .. } => require!(matches!(kind, Kind::Uint(_)) && den.0 > 0, format!("argument {i}: floor needs a number and den > 0")),
        }
    }
    for (i, rule) in t.args.iter().enumerate() {
        if let Arg::Floor { of, .. } = rule {
            require!(matches!(t.args.get(*of as usize), Some(Arg::AtMost { .. })), format!("argument {i}: a floor is relative to an at_most amount"));
        }
    }
    if let Some(sp) = t.spends.as_mut() {
        sp.token = sp.token.to_lowercase();
        require!(parse_address(&sp.token).is_some(), "spends: a token address");
        require!(matches!(t.args.get(sp.arg as usize), Some(Arg::AtMost { .. })), "spends: an at_most amount");
    }
}

fn addr_word(a: &evmtx::Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// A number as a word of type `kind` (refused if it doesn't fit).
fn uint_word(v: u128, kind: Kind) -> [u8; 32] {
    let bits = match kind {
        Kind::Uint(b) => b,
        Kind::Bool => 1,
        Kind::Address => env::panic_str("a number where an address goes"),
    };
    require!(bits >= 128 || v >> bits == 0, "refused: number too big for its type");
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn fixed_word(value: &str, kind: Kind) -> [u8; 32] {
    match kind {
        Kind::Address => addr_word(&parse_address(value).unwrap_or_else(|| env::panic_str("fixed: a 0x address"))),
        _ => uint_word(value.parse().unwrap_or_else(|_| env::panic_str("fixed: a decimal number")), kind),
    }
}
