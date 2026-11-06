//! EVM chains through NEAR Chain Signatures, as vault/src/evm.rs: the brain
//! BUILDS every transaction from a template the owner set (rule trees over the
//! ABI: fixed / vault / at_most / floor / market / same / bytes / encoded /
//! tuple / list), applies caps, floors and market minimums, counts spending,
//! and answers with the vault's `sign` call (the MPC key is derived from the
//! vault's account, so the EVM address is still per vault). The signature
//! comes back through the vault (`on_vault` EvmSigned): the brain assembles the
//! signed transaction, emits `evm_signed` and returns it to the keeper.
//!
//! What changed in the split: the vault's address (a `derived_public_key`
//! view taking the vault as predecessor) and market prices are read by the
//! brain itself. The pure part below (rules, checks, encoding, low-s) is the
//! vault's code verbatim.

use evmtx::enc::{encode, Val};
use evmtx::sig::{self, Ty};
use evmtx::{abi, address_of, hex_address, parse_address, Eip1559};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use crate::wire::Answer;
use crate::{Next, Read, Reply, Talk, GAS_READ};

pub const EVM_PATH: &str = "evm-1";
const ECDSA_DOMAIN: u32 = 0;
const GAS_SIGN: Gas = Gas::from_tgas(50);
/// on_evm_signed: burns 4.9 Tgas (it encodes the signed transaction); 12 is a ~2.5x margin.
pub(crate) const GAS_SIGNED: Gas = Gas::from_tgas(12);
/// Reading prices, then building and checking the transaction.
/// on_read after the oracle: builds and checks the transaction, then answers the
/// signature request (no promises of its own); burns ~5.3 Tgas, 14 is a ~2.5x margin.
pub(crate) const GAS_BUILD: Gas = Gas::from_tgas(14);
const MAX_TEMPLATES: usize = 32;
const N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe, 0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e,
    0x8c, 0xd0, 0x36, 0x41, 0x41,
];

#[near(serializers = [borsh, json])]
#[serde(tag = "rule", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Fixed { value: String },
    Vault,
    AtMost { max: U128 },
    Floor { of: u8, num: U128, den: U128 },
    Market { of: u8, num: U128, den: U128, market: crate::oracle::Market },
    Same { of: u8 },
    Bytes { hex: String },
    Encoded { types: String, args: Vec<Arg> },
    Tuple { args: Vec<Arg> },
    List { items: Vec<Arg> },
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub arg: u8,
    pub token: String,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmTemplate {
    pub name: String,
    pub to: String,
    pub function: String,
    pub args: Vec<Arg>,
    #[serde(default)]
    pub value: Option<Arg>,
    pub spends: Option<Spend>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmConfig {
    pub signer: AccountId,
    pub chain_id: u64,
    pub address: Option<String>,
    pub templates: Vec<EvmTemplate>,
    pub max_fee: U128,
    pub max_tip: U128,
    pub gas_cap: u64,
}

/// A transaction the brain built, carried to the signature callback.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct TxJson {
    nonce: u64,
    max_priority_fee: U128,
    max_fee: U128,
    gas_limit: u64,
    to: String,
    value: U128,
    data: String,
}

/// The keeper's numbers for a template call.
#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct EvmArgs {
    pub name: String,
    pub values: Vec<U128>,
    pub nonce: u64,
    pub max_fee: U128,
    pub max_tip: U128,
    pub gas: u64,
}

impl Talk {
    pub(crate) fn evm_setup(&mut self, signer: AccountId, chain_id: u64, max_fee: U128, max_tip: U128, gas_cap: u64) -> Reply {
        require!(gas_cap <= 2_000_000, "gas cap: at most 2M");
        let templates = self.vs.evm.as_ref().filter(|c| c.chain_id == chain_id).map(|c| c.templates.clone()).unwrap_or_default();
        self.vs.evm = Some(EvmConfig { signer: signer.clone(), chain_id, address: None, templates, max_fee, max_tip, gas_cap });
        let args = near_sdk::serde_json::json!({"path": EVM_PATH, "predecessor": self.vault, "domain_id": ECDSA_DOMAIN}).to_string();
        self.read(vec![(signer, "derived_public_key".into(), args)], Read::EvmAddress, GAS_READ)
    }

    pub(crate) fn on_evm_address(&mut self) -> Reply {
        let raw = crate::result(0, 512).unwrap_or_else(|| env::panic_str("the signer didn't answer"));
        let key: String = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("bad key"));
        let b58 = key.strip_prefix("secp256k1:").unwrap_or_else(|| env::panic_str("expected a secp256k1 key"));
        let pk = near_sdk::bs58::decode(b58).into_vec().unwrap_or_else(|_| env::panic_str("bad key"));
        let addr = hex_address(&address_of(&pk).unwrap_or_else(|| env::panic_str("bad key length")));
        self.vs.evm.as_mut().unwrap().address = Some(addr.clone());
        self.emit("evm_address", &format!(r#""address":"{addr}""#));
        Answer::default().ret(&format!(r#""{addr}""#)).into()
    }

    pub(crate) fn evm_template(&mut self, template: EvmTemplate) {
        let mut t = template;
        t.to = t.to.to_lowercase();
        check_template(&mut t);
        let cfg = self.vs.evm.as_mut().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        cfg.templates.retain(|x| x.name != t.name);
        require!(cfg.templates.len() < MAX_TEMPLATES, "too many templates");
        cfg.templates.push(t);
    }

    pub(crate) fn evm_template_remove(&mut self, name: String) {
        if let Some(cfg) = self.vs.evm.as_mut() {
            cfg.templates.retain(|x| x.name != name);
        }
    }

    pub(crate) fn evm_template_named(&self, name: &str) -> EvmTemplate {
        let cfg = self.vs.evm.as_ref().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        cfg.templates.iter().find(|t| t.name == name).cloned().unwrap_or_else(|| env::panic_str("refused: no such template"))
    }

    /// Owner or keeper: build, check and have template `a.name` signed.
    pub(crate) fn evm_call(&mut self, a: EvmArgs) -> Reply {
        let t = self.evm_template_named(&a.name);
        let feeds = market_feeds(&leaves(&t));
        if feeds.is_empty() {
            return self.evm_build(a, &[]);
        }
        // Market minimums: the brain reads the prices, then builds in `on_read`.
        let views = self.feed_views(&feeds);
        self.need_after = GAS_SIGN.as_tgas() + GAS_SIGNED.as_tgas() + crate::RELAY_TGAS;
        self.read(views, Read::EvmPrices(a), GAS_BUILD)
    }

    pub(crate) fn on_evm_prices(&mut self, a: EvmArgs) -> Reply {
        let feeds = market_feeds(&leaves(&self.evm_template_named(&a.name)));
        let prices = self.named_prices(&feeds, 0);
        self.evm_build(a, &prices)
    }

    fn evm_build(&mut self, a: EvmArgs, prices: &[(String, u128)]) -> Reply {
        let cfg = self.vs.evm.clone().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        let t = self.evm_template_named(&a.name);
        let (tx, _) = self.evm_prepare(&cfg, &t, a.values, a.nonce, a.max_fee, a.max_tip, a.gas, prices);
        let (ans, carry) = evm_sign_call(&cfg.signer, &tx);
        self.charge_signature(ans).then("on_vault", near_sdk::borsh::to_vec(&Next::EvmSigned(carry)).unwrap(), GAS_SIGNED).into()
    }

    /// Build template `t` with `values`: caps, floors and market minimums applied,
    /// spending counted. Returns the unsigned transaction and each leaf's number.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn evm_prepare(&mut self, cfg: &EvmConfig, t: &EvmTemplate, values: Vec<U128>, nonce: u64, max_fee: U128, max_tip: U128, gas: u64, prices: &[(String, u128)]) -> (Eip1559, Vec<Option<u128>>) {
        let name = t.name.clone();
        let me = parse_address(cfg.address.as_deref().unwrap_or_else(|| env::panic_str("the vault's EVM address isn't known yet"))).unwrap();
        let rules = leaves(t);
        require!(values.len() == rules.len(), format!("one value per leaf ({})", rules.len()));
        let nums = numbers(&rules, &values, prices);
        let tys = sig::args(&t.function).unwrap();
        let mut at = 0;
        let vals: Vec<Val> = t.args.iter().zip(&tys).map(|(r, ty)| build(r, ty, &me, &nums, &mut at)).collect();
        let mut data = abi::selector(&t.function).to_vec();
        data.extend(encode(&tys, &vals));
        let value = if t.value.is_some() { nums[rules.len() - 1].unwrap_or(0) } else { 0 };
        if let Some(sp) = &t.spends {
            self.charge(&format!("evm:{}:{}", cfg.chain_id, sp.token), nums[sp.arg as usize].unwrap_or(0));
        }
        if value > 0 {
            self.charge(&format!("evm:{}:eth", cfg.chain_id), value);
        }
        require!(max_fee.0 <= cfg.max_fee.0 && max_tip.0 <= cfg.max_tip.0 && max_tip.0 <= max_fee.0, "refused: gas price over the owner's cap");
        require!(gas <= cfg.gas_cap, "refused: gas over the owner's cap");
        let tx = Eip1559 { chain_id: cfg.chain_id, nonce, max_priority_fee: max_tip.0, max_fee: max_fee.0, gas_limit: gas, to: parse_address(&t.to).unwrap(), value, data };
        self.emit("evm_call_requested", &format!(r#""template":"{name}","to":"{}","value":"{value}","data":"0x{}""#, t.to, hex::encode(&tx.data)));
        (tx, nums)
    }

    /// The vault's `sign` came back for `tx`: emit `evm_signed` (or `evm_sign_failed`).
    /// Returns the signed event's JSON, if signed.
    pub(crate) fn evm_emit_signed(&self, tx: &TxJson) -> Option<String> {
        let cfg = self.vs.evm.clone().unwrap();
        let t = Eip1559 {
            chain_id: cfg.chain_id, nonce: tx.nonce, max_priority_fee: tx.max_priority_fee.0, max_fee: tx.max_fee.0, gas_limit: tx.gas_limit,
            to: parse_address(&tx.to).unwrap(), value: tx.value.0, data: hex::decode(&tx.data).unwrap(),
        };
        let hash = hex::encode(t.signing_hash());
        match crate::result(0, 4096).and_then(|raw| ecdsa_from(&raw)) {
            Some((r, s, v)) => {
                let raw = hex::encode(t.signed(v, &r, &s));
                let data = format!(r#""hash":"0x{hash}","raw":"0x{raw}","r":"{}","s":"{}","v":{v}"#, hex::encode(r), hex::encode(s));
                self.emit("evm_signed", &data);
                Some(format!("{{{data}}}"))
            }
            None => {
                self.emit("evm_sign_failed", &format!(r#""hash":"0x{hash}""#));
                None
            }
        }
    }

    pub(crate) fn on_evm_signed(&mut self, tx: TxJson) -> Reply {
        let signed = self.evm_emit_signed(&tx);
        Answer::default().ret(&signed.unwrap_or_else(|| "null".into())).into()
    }
}

/// The vault's `sign` call for `t`, and the transaction to carry to its callback.
pub(crate) fn evm_sign_call(signer: &AccountId, t: &Eip1559) -> (Answer, TxJson) {
    let args = near_sdk::serde_json::json!({"request": {"path": EVM_PATH, "payload_v2": {"Ecdsa": hex::encode(t.signing_hash())}, "domain_id": ECDSA_DOMAIN}}).to_string();
    let carry = TxJson { nonce: t.nonce, max_priority_fee: U128(t.max_priority_fee), max_fee: U128(t.max_fee), gas_limit: t.gas_limit, to: hex_address(&t.to), value: U128(t.value), data: hex::encode(&t.data) };
    (Answer::default().call(signer, "sign", args.as_bytes(), 1, GAS_SIGN), carry)
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

/// What a leaf is, for checking cross references.
#[derive(Clone, Copy, PartialEq)]
enum Leaf {
    Amount,
    Min,
    Same,
    Other,
}

/// Check a template once, when the owner saves it: a parsable function, rules
/// shaped like its types, every leaf fitting its type, and references (floor,
/// same) pointing at an amount or a minimum, never at another `same`.
fn check_template(t: &mut EvmTemplate) {
    require!(!t.name.is_empty() && t.name.len() <= 40, "name: 1 to 40 characters");
    require!(parse_address(&t.to).is_some(), "to: a 0x address");
    let tys = sig::args(&t.function).unwrap_or_else(|| env::panic_str("function: a signature of address/uintN/intN/bool/bytes, tuples and T[]"));
    require!(tys.len() == t.args.len(), "one rule per argument");
    let mut kinds = Vec::new();
    for (r, ty) in t.args.iter_mut().zip(&tys) {
        check_rule(r, ty, &mut kinds, 0);
    }
    if let Some(v) = t.value.as_mut() {
        require!(matches!(v, Arg::AtMost { .. } | Arg::Same { .. }), "value: at_most or same");
        check_rule(v, &Ty::Uint(256), &mut kinds, 0);
    }
    require!(kinds.len() <= 64, "at most 64 leaves");
    let rules: Vec<Arg> = leaves(t);
    for (i, r) in rules.iter().enumerate() {
        match r {
            Arg::Floor { of, .. } | Arg::Market { of, .. } => require!(kinds.get(*of as usize) == Some(&Leaf::Amount), format!("leaf {i}: a floor is relative to an at_most amount")),
            Arg::Same { of } => require!(matches!(kinds.get(*of as usize), Some(Leaf::Amount | Leaf::Min)), format!("leaf {i}: same as an at_most or floor number")),
            _ => {}
        }
    }
    if let Some(sp) = t.spends.as_mut() {
        sp.token = sp.token.to_lowercase();
        require!(parse_address(&sp.token).is_some(), "spends: a token address");
        require!(kinds.get(sp.arg as usize) == Some(&Leaf::Amount), "spends: an at_most amount");
    }
}

fn check_rule(r: &mut Arg, ty: &Ty, kinds: &mut Vec<Leaf>, depth: u8) {
    require!(depth < 8, "rules nested too deep");
    let i = kinds.len();
    match (r, ty) {
        (Arg::Fixed { value }, _) => {
            *value = value.to_lowercase();
            let _ = fixed_word(value, ty);
            kinds.push(Leaf::Other);
        }
        (Arg::Vault, Ty::Address) => kinds.push(Leaf::Other),
        (Arg::AtMost { .. }, Ty::Uint(_)) => kinds.push(Leaf::Amount),
        (Arg::Floor { den, .. }, Ty::Uint(_)) => {
            require!(den.0 > 0, format!("leaf {i}: den > 0"));
            kinds.push(Leaf::Min)
        }
        (Arg::Market { den, market, .. }, Ty::Uint(_)) => {
            require!(den.0 > 0 && market.slippage_bps <= 5_000 && market.dec_in <= 36 && market.dec_out <= 36, format!("leaf {i}: den > 0, slippage <= 50%, decimals <= 36"));
            kinds.push(Leaf::Min)
        }
        (Arg::Same { .. }, Ty::Uint(_)) => kinds.push(Leaf::Same),
        (Arg::Bytes { hex: h }, Ty::Bytes) => {
            *h = h.to_lowercase();
            let b = hex::decode(h.trim_start_matches("0x")).unwrap_or_else(|_| env::panic_str("bytes: hex"));
            require!(b.len() <= 1024, "bytes: at most 1 KB");
            kinds.push(Leaf::Other);
        }
        (Arg::Encoded { types, args }, Ty::Bytes) => {
            let tys = sig::list(types).unwrap_or_else(|| env::panic_str("encoded: types like (address,uint256)"));
            require!(tys.len() == args.len(), "encoded: one rule per type");
            for (r, t) in args.iter_mut().zip(&tys) {
                check_rule(r, t, kinds, depth + 1);
            }
        }
        (Arg::Tuple { args }, Ty::Tuple(tys)) => {
            require!(tys.len() == args.len(), "tuple: one rule per field");
            for (r, t) in args.iter_mut().zip(tys) {
                check_rule(r, t, kinds, depth + 1);
            }
        }
        (Arg::List { items }, Ty::Array(t)) => {
            require!(items.len() <= 16, "list: at most 16 items");
            for r in items.iter_mut() {
                check_rule(r, t, kinds, depth + 1);
            }
        }
        (r, ty) => env::panic_str(&format!("leaf {i}: rule {} doesn't fit type {ty:?}", rule_name(r))),
    }
}

fn rule_name(r: &Arg) -> &'static str {
    match r {
        Arg::Fixed { .. } => "fixed",
        Arg::Vault => "vault",
        Arg::AtMost { .. } => "at_most",
        Arg::Floor { .. } => "floor",
        Arg::Market { .. } => "market",
        Arg::Same { .. } => "same",
        Arg::Bytes { .. } => "bytes",
        Arg::Encoded { .. } => "encoded",
        Arg::Tuple { .. } => "tuple",
        Arg::List { .. } => "list",
    }
}

/// The template's leaf rules, depth first, the value leaf last.
pub(crate) fn leaves(t: &EvmTemplate) -> Vec<Arg> {
    fn walk(r: &Arg, out: &mut Vec<Arg>) {
        match r {
            Arg::Encoded { args, .. } | Arg::Tuple { args } => args.iter().for_each(|a| walk(a, out)),
            Arg::List { items } => items.iter().for_each(|a| walk(a, out)),
            leaf => out.push(leaf.clone()),
        }
    }
    let mut out = Vec::new();
    t.args.iter().chain(t.value.iter()).for_each(|r| walk(r, &mut out));
    out
}

/// The feeds a template's market leaves need, each once.
pub(crate) fn market_feeds(rules: &[Arg]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for r in rules {
        if let Arg::Market { market, .. } = r {
            for f in [&market.sell, &market.buy] {
                if !out.contains(f) {
                    out.push(f.clone());
                }
            }
        }
    }
    out
}

/// The number each numeric leaf ends up with: amounts capped, minimums at least
/// their floor (and the market's, from `prices`), `same` copied. None for non-numeric leaves.
fn numbers(rules: &[Arg], values: &[U128], prices: &[(String, u128)]) -> Vec<Option<u128>> {
    let mut nums: Vec<Option<u128>> = vec![None; rules.len()];
    for (i, r) in rules.iter().enumerate() {
        if let Arg::AtMost { max } = r {
            require!(values[i].0 <= max.0, format!("refused: leaf {i} over its cap"));
            nums[i] = Some(values[i].0);
        }
    }
    for (i, r) in rules.iter().enumerate() {
        match r {
            Arg::Floor { of, num, den } => {
                let floor = crate::mul_div(nums[*of as usize].unwrap_or(0), num.0, den.0);
                nums[i] = Some(values[i].0.max(floor));
            }
            Arg::Market { of, num, den, market } => {
                let amount = nums[*of as usize].unwrap_or(0);
                let price = |f: &String| prices.iter().find(|(n, _)| n == f).map(|(_, p)| *p).unwrap_or_else(|| env::panic_str("refused: no price"));
                let floor = crate::mul_div(amount, num.0, den.0).max(crate::oracle::market_min(market, amount, price(&market.sell), price(&market.buy)));
                nums[i] = Some(values[i].0.max(floor));
            }
            _ => {}
        }
    }
    for (i, r) in rules.iter().enumerate() {
        if let Arg::Same { of } = r {
            nums[i] = nums[*of as usize];
        }
    }
    nums
}

/// The value of rule `r` for type `ty`; `at` counts leaves depth first.
fn build(r: &Arg, ty: &Ty, me: &evmtx::Address, nums: &[Option<u128>], at: &mut usize) -> Val {
    match (r, ty) {
        (Arg::Encoded { types, args }, Ty::Bytes) => {
            let tys = sig::list(types).unwrap();
            let vals: Vec<Val> = args.iter().zip(&tys).map(|(r, t)| build(r, t, me, nums, at)).collect();
            Val::Bytes(encode(&tys, &vals))
        }
        (Arg::Tuple { args }, Ty::Tuple(tys)) => Val::Tuple(args.iter().zip(tys).map(|(r, t)| build(r, t, me, nums, at)).collect()),
        (Arg::List { items }, Ty::Array(t)) => Val::Array(items.iter().map(|r| build(r, t, me, nums, at)).collect()),
        (leaf, _) => {
            let i = *at;
            *at += 1;
            match leaf {
                Arg::Fixed { value } => Val::Word(fixed_word(value, ty)),
                Arg::Vault => Val::Word(addr_word(me)),
                Arg::Bytes { hex: h } => Val::Bytes(hex::decode(h.trim_start_matches("0x")).unwrap()),
                _ => Val::Word(uint_word(nums[i].unwrap_or(0), ty)),
            }
        }
    }
}

fn addr_word(a: &evmtx::Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a);
    w
}

/// A number as a word of type `ty` (refused if it doesn't fit).
fn uint_word(v: u128, ty: &Ty) -> [u8; 32] {
    let bits = match ty {
        Ty::Uint(b) => *b,
        Ty::Bool => 1,
        _ => env::panic_str("a number where something else goes"),
    };
    require!(bits >= 128 || v >> bits == 0, "refused: number too big for its type");
    let mut w = [0u8; 32];
    w[16..].copy_from_slice(&v.to_be_bytes());
    w
}

fn fixed_word(value: &str, ty: &Ty) -> [u8; 32] {
    match ty {
        Ty::Address => addr_word(&parse_address(value).unwrap_or_else(|| env::panic_str("fixed: a 0x address"))),
        Ty::Bool => uint_word(match value { "true" | "1" => 1, "false" | "0" => 0, _ => env::panic_str("fixed: a bool") }, ty),
        Ty::Uint(_) => uint_word(value.parse().unwrap_or_else(|_| env::panic_str("fixed: a decimal number")), ty),
        Ty::Int(bits) => {
            let v: i128 = value.parse().unwrap_or_else(|_| env::panic_str("fixed: a decimal number"));
            let lim = if *bits >= 128 { i128::MAX } else { (1i128 << (bits - 1)) - 1 };
            require!(v <= lim && v >= -lim - 1, "fixed: number too big for its type");
            let mut w = if v < 0 { [0xffu8; 32] } else { [0u8; 32] };
            w[16..].copy_from_slice(&v.to_be_bytes());
            w
        }
        _ => env::panic_str("fixed: only addresses, numbers and bools"),
    }
}
