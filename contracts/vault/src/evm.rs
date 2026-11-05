//! EVM chains from the vault, through NEAR Chain Signatures (secp256k1).
//!
//! Unlike Solana, the vault BUILDS these transactions itself (evmtx): an ERC-20
//! approve for the router, or a Uniswap exactInputSingle whose recipient is the
//! vault's own EVM address and whose minimum output is at least the owner's
//! floor. The keeper only supplies numbers that can't redirect money: nonce,
//! gas prices (capped), and a quote (the floor still applies). The vault signs
//! the hash, assembles the signed transaction, and emits it for broadcast.

use evmtx::{abi, address_of, hex_address, parse_address, Eip1559};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise};

use crate::{Vault, VaultExt};

pub const EVM_PATH: &str = "evm-1";
const ECDSA_DOMAIN: u32 = 0;
const GAS_SIGN: Gas = Gas::from_tgas(50);
const GAS_CB: Gas = Gas::from_tgas(15);
const APPROVE_GAS: u64 = 80_000;
/// secp256k1 group order n, big-endian (for the low-s rule, EIP-2).
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e,
    0x8c, 0xd0, 0x36, 0x41, 0x41,
];

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmPair {
    /// 0x-prefixed token addresses
    pub token_in: String,
    pub token_out: String,
    /// Uniswap pool fee tier (500, 3000, 10000)
    pub fee: u32,
    pub max_in: U128,
    /// least output per input, scaled 1e12
    pub min_out_per_in_e12: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmConfig {
    pub signer: AccountId,
    pub chain_id: u64,
    /// The vault's EVM address, from the signer.
    pub address: Option<String>,
    /// Uniswap SwapRouter02 on this chain.
    pub router: String,
    pub pairs: Vec<EvmPair>,
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
    /// Owner: one EVM chain, its Uniswap router, and fee caps. Learns the vault's address from the signer.
    pub fn evm_setup(&mut self, signer: AccountId, chain_id: u64, router: String, max_fee: U128, max_tip: U128, gas_cap: u64) -> Promise {
        self.assert_owner();
        require!(parse_address(&router.to_lowercase()).is_some(), "router: a 0x address");
        require!(gas_cap <= 1_000_000, "gas cap: at most 1M");
        self.evm = Some(EvmConfig { signer: signer.clone(), chain_id, address: None, router: router.to_lowercase(), pairs: vec![], max_fee, max_tip, gas_cap });
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

    /// Owner: allow a pair, or remove it with `max_in: "0"`.
    pub fn evm_allow(&mut self, pair: EvmPair) {
        self.assert_owner();
        let mut pair = pair;
        pair.token_in = pair.token_in.to_lowercase();
        pair.token_out = pair.token_out.to_lowercase();
        require!(parse_address(&pair.token_in).is_some() && parse_address(&pair.token_out).is_some(), "tokens: 0x addresses");
        let cfg = self.evm.as_mut().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        cfg.pairs.retain(|p| !(p.token_in == pair.token_in && p.token_out == pair.token_out));
        if pair.max_in.0 > 0 {
            require!(cfg.pairs.len() < 16, "too many pairs");
            cfg.pairs.push(pair);
        }
    }

    /// Owner or keeper: approve the router to spend `amount` of `token_in` (an allowed pair's input).
    pub fn evm_approve(&mut self, token_in: String, amount: U128, nonce: u64, max_fee: U128, max_tip: U128) -> Promise {
        let cfg = self.evm_caller_cfg();
        let token_in = token_in.to_lowercase();
        let pair = cfg.pairs.iter().find(|p| p.token_in == token_in).unwrap_or_else(|| env::panic_str("refused: no allowed pair spends this token"));
        require!(amount.0 <= pair.max_in.0, "refused: more than one swap's limit");
        let router = parse_address(&cfg.router).unwrap();
        let tx = self.evm_tx(&cfg, nonce, max_fee, max_tip, APPROVE_GAS, parse_address(&token_in).unwrap(), abi::approve(&router, amount.0));
        self.evm_sign(cfg.signer, tx)
    }

    /// Owner or keeper: swap on Uniswap. `min_out` is the keeper's quote-based minimum;
    /// the transaction uses the HIGHER of it and the owner's floor.
    #[allow(clippy::too_many_arguments)]
    pub fn evm_swap(&mut self, token_in: String, token_out: String, amount_in: U128, min_out: U128, nonce: u64, max_fee: U128, max_tip: U128, gas: u64) -> Promise {
        let cfg = self.evm_caller_cfg();
        let (ti, to) = (token_in.to_lowercase(), token_out.to_lowercase());
        let pair = cfg.pairs.iter().find(|p| p.token_in == ti && p.token_out == to).cloned().unwrap_or_else(|| env::panic_str("refused: pair not allowed"));
        require!(amount_in.0 > 0 && amount_in.0 <= pair.max_in.0, "refused: amount over the pair's limit");
        let floor = crate::mul_div(amount_in.0, pair.min_out_per_in_e12.0, 1_000_000_000_000);
        let out_min = min_out.0.max(floor);
        self.charge(&format!("evm:{}:{ti}", cfg.chain_id), amount_in.0);
        let me = parse_address(cfg.address.as_deref().unwrap_or_else(|| env::panic_str("the vault's EVM address isn't known yet"))).unwrap();
        let data = abi::exact_input_single(&parse_address(&ti).unwrap(), &parse_address(&to).unwrap(), pair.fee, &me, amount_in.0, out_min);
        let tx = self.evm_tx(&cfg, nonce, max_fee, max_tip, gas, parse_address(&cfg.router).unwrap(), data);
        self.emit("evm_swap_requested", &format!(r#"{{"token_in":"{ti}","token_out":"{to}","amount_in":"{}","min_out":"{out_min}"}}"#, amount_in.0));
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
