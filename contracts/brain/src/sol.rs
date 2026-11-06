//! Solana through NEAR Chain Signatures, as vault/src/sol.rs and
//! soltemplates.rs: the keeper brings a message (a Jupiter swap, or a
//! transaction made only of the owner's instruction templates: pump.fun,
//! Meteora, pump AMM...); the brain checks every byte with solguard, counts the
//! spending READ FROM THE MESSAGE, and answers with the vault's `sign` call
//! (the Solana address derives from the vault's account, so it stays per
//! vault). The signature comes back through the vault (`on_vault` SolSigned):
//! emitted as `sol_signed` and returned to the keeper. `needs_owner` templates
//! still need the owner's own wallet signature before they can land.
//!
//! What changed in the split: the vault's address and market prices are read
//! by the brain itself. The template types below are the vault's verbatim.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};
use solguard::template::{Acc, Field, FieldRule, Template, Ty};

use crate::wire::Answer;
use crate::{Next, Read, Reply, Talk, GAS_READ};

/// A field rule, as JSON.
#[near(serializers = [borsh, json])]
#[serde(tag = "rule", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum FieldJ {
    /// Little-endian bytes, hex.
    Fixed { hex: String },
    Any,
    AtMost { max: U128 },
    AtLeast { of: u8, num: U128, den: U128 },
    /// At least the larger of `field[of] * num / den` and what the oracle says `field[of]` is
    /// worth, less slippage (the EVM `market` leaf): the brain reads the prices before it checks.
    Market { of: u8, num: U128, den: U128, market: crate::oracle::Market },
}

#[near(serializers = [borsh, json])]
#[serde(tag = "acc", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum AccJ {
    Any,
    Fixed { key: String },
    Vault,
    VaultAta { mint_at: u8, program_at: u8 },
    /// The vault's token account for a mint the instruction doesn't name (base58 mint and token program).
    VaultAtaOf { mint: String, program: String },
    Owner,
    NewSigner,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolField {
    /// u8 u16 u32 u64 u128 i32 i64 bool, or bytesN (e.g. bytes64).
    pub ty: String,
    #[serde(flatten)]
    pub rule: FieldJ,
}

/// What a template spends: field `field`, counted as spending limit `key` (e.g. "sol:SOL").
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolSpend {
    pub field: u8,
    pub key: String,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolTemplate {
    pub name: String,
    /// base58
    pub program: String,
    /// The instruction's first bytes (Anchor discriminator), hex.
    pub prefix: String,
    pub fields: Vec<SolField>,
    pub accounts: Vec<AccJ>,
    #[serde(default)]
    pub extra_accounts: u8,
    #[serde(default)]
    pub needs_owner: bool,
    #[serde(default)]
    pub spends: Vec<SolSpend>,
}

fn ty(s: &str) -> Ty {
    match s {
        "u8" => Ty::U8,
        "u16" => Ty::U16,
        "u32" => Ty::U32,
        "u64" => Ty::U64,
        "u128" => Ty::U128,
        "i32" => Ty::I32,
        "i64" => Ty::I64,
        "bool" => Ty::Bool,
        _ => Ty::Bytes(s.strip_prefix("bytes").and_then(|n| n.parse().ok()).filter(|n| *n <= 256).unwrap_or_else(|| env::panic_str(&format!("field type {s}")))),
    }
}

fn key(b58: &str) -> [u8; 32] {
    near_sdk::bs58::decode(b58).into_vec().ok().and_then(|v| v.try_into().ok()).unwrap_or_else(|| env::panic_str(&format!("not a Solana address: {b58}")))
}

/// A market floor's ratio per unit of input, scaled (amounts are u64: 1e24 keeps it exact to a unit).
const MARKET_SCALE: u128 = 1_000_000_000_000_000_000_000_000;

/// The larger of two minimums per unit of input, `a_num / a_den` and `b_num / b_den`.
fn larger(a: (u128, u128), b: (u128, u128)) -> (u128, u128) {
    use math::wide::U256;
    if U256::from(a.0) * U256::from(b.1) >= U256::from(b.0) * U256::from(a.1) { a } else { b }
}

impl SolTemplate {
    /// The feeds this template's market fields need.
    pub fn market_feeds(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in &self.fields {
            if let FieldJ::Market { market, .. } = &f.rule {
                for n in [&market.sell, &market.buy] {
                    if !out.contains(n) {
                        out.push(n.clone());
                    }
                }
            }
        }
        out
    }

    /// The template solguard checks. `prices` (feed, USD 1e18): market fields become at-least
    /// rules at the larger of their fixed ratio and the market's; a missing price refuses. Without
    /// prices (saving, a first look at a message) a market field is its fixed ratio alone.
    pub fn to_guard(&self, prices: Option<&[(String, u128)]>) -> Template {
        let price = |f: &String| prices.unwrap_or(&[]).iter().find(|(n, _)| n == f).map(|(_, p)| *p).unwrap_or_else(|| env::panic_str("refused: no price"));
        Template {
            name: self.name.clone(),
            program: key(&self.program),
            prefix: hex::decode(&self.prefix).unwrap_or_else(|_| env::panic_str("prefix: hex")),
            fields: self
                .fields
                .iter()
                .map(|f| Field {
                    ty: ty(&f.ty),
                    rule: match &f.rule {
                        FieldJ::Fixed { hex: h } => FieldRule::Fixed(hex::decode(h).unwrap_or_else(|_| env::panic_str("fixed: hex"))),
                        FieldJ::Any => FieldRule::Any,
                        FieldJ::AtMost { max } => FieldRule::AtMost(max.0),
                        FieldJ::AtLeast { of, num, den } => FieldRule::AtLeast { of: *of, num: num.0, den: den.0 },
                        FieldJ::Market { of, num, den, market } => {
                            let (num, den) = match prices {
                                None => (num.0, den.0),
                                Some(_) => {
                                    let per_in = crate::oracle::market_per_in(market, price(&market.sell), price(&market.buy), MARKET_SCALE);
                                    larger((num.0, den.0), (per_in, MARKET_SCALE))
                                }
                            };
                            FieldRule::AtLeast { of: *of, num, den }
                        }
                    },
                })
                .collect(),
            accounts: self
                .accounts
                .iter()
                .map(|a| match a {
                    AccJ::Any => Acc::Any,
                    AccJ::Fixed { key: k } => Acc::Fixed(key(k)),
                    AccJ::Vault => Acc::Vault,
                    AccJ::VaultAta { mint_at, program_at } => Acc::VaultAta { mint_at: *mint_at, program_at: *program_at },
                    AccJ::VaultAtaOf { mint, program } => Acc::VaultAtaOf { mint: key(mint), program: key(program) },
                    AccJ::Owner => Acc::Owner,
                    AccJ::NewSigner => Acc::NewSigner,
                })
                .collect(),
            extra_accounts: self.extra_accounts,
            needs_owner: self.needs_owner,
        }
    }
}


pub const SOL_PATH: &str = "solana-1";
pub(crate) const EDDSA_DOMAIN: u32 = 1;
pub(crate) const GAS_SIGN: Gas = Gas::from_tgas(50);
/// on_sol_signed: burns 2.5 Tgas; 8 is a ~3x margin.
pub(crate) const GAS_SIGNED: Gas = Gas::from_tgas(8);
/// Reading prices, then checking the message again against the market floor.
/// (on_read: checks the message again with the prices, answers the signature
/// request; burns 4.0 Tgas, 12 is a 3x margin.)
const GAS_CHECK: Gas = Gas::from_tgas(12);

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolPair {
    pub in_mint: String,
    pub out_mint: String,
    pub max_in: U128,
    pub min_out_per_in_e12: U128,
    #[serde(default)]
    pub market: Option<crate::oracle::Market>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct SolConfig {
    pub signer: AccountId,
    pub address: Option<String>,
    pub pairs: Vec<SolPair>,
    pub max_cu_price: u64,
    pub max_cu_limit: u32,
    #[serde(default)]
    pub templates: Vec<SolTemplate>,
    #[serde(default)]
    pub owner_wallet: Option<String>,
}

pub(crate) fn decode_message(message: &str) -> Vec<u8> {
    let bytes = near_sdk::base64::Engine::decode(&near_sdk::base64::engine::general_purpose::STANDARD, message).unwrap_or_else(|_| env::panic_str("message: base64"));
    require!(bytes.len() <= 1232, "a Solana message fits in 1232 bytes");
    bytes
}

/// The vault's `sign` call for a Solana message (the MPC key derives from the vault).
pub(crate) fn sol_sign_call(signer: &AccountId, bytes: &[u8]) -> Answer {
    let args = near_sdk::serde_json::json!({"request": {"path": SOL_PATH, "payload_v2": {"Eddsa": hex::encode(bytes)}, "domain_id": EDDSA_DOMAIN}}).to_string();
    Answer::default().call(signer, "sign", args.as_bytes(), 1, GAS_SIGN)
}

impl Talk {
    pub(crate) fn sol_cfg(&self) -> SolConfig {
        self.vs.sol.clone().unwrap_or_else(|| env::panic_str("run sol_setup first"))
    }

    pub(crate) fn sol_setup(&mut self, signer: AccountId, max_cu_price: u64, max_cu_limit: u32) -> Reply {
        require!(max_cu_limit <= 1_400_000, "compute limit: at most 1.4M");
        self.vs.sol = Some(SolConfig { signer: signer.clone(), address: None, pairs: vec![], max_cu_price, max_cu_limit, templates: vec![], owner_wallet: None });
        let args = near_sdk::serde_json::json!({"path": SOL_PATH, "predecessor": self.vault, "domain_id": EDDSA_DOMAIN}).to_string();
        self.read(vec![(signer, "derived_public_key".into(), args)], Read::SolAddress, GAS_READ)
    }

    pub(crate) fn on_sol_address(&mut self) -> Reply {
        let raw = crate::result(0, 512).unwrap_or_else(|| env::panic_str("the signer didn't answer"));
        let key: String = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("bad key"));
        let b58 = key.strip_prefix("ed25519:").unwrap_or_else(|| env::panic_str("expected an ed25519 key")).to_string();
        let bytes = near_sdk::bs58::decode(&b58).into_vec().unwrap_or_else(|_| env::panic_str("bad key"));
        require!(bytes.len() == 32, "bad key length");
        self.vs.sol.as_mut().unwrap().address = Some(b58.clone());
        self.emit("sol_address", &format!(r#""address":"{b58}""#));
        Answer::default().ret(&format!(r#""{b58}""#)).into()
    }

    pub(crate) fn sol_allow(&mut self, pair: SolPair) {
        for m in [&pair.in_mint, &pair.out_mint] {
            require!(near_sdk::bs58::decode(m).into_vec().map_or(false, |v| v.len() == 32), "mints are base58 addresses");
        }
        let cfg = self.vs.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.pairs.retain(|p| !(p.in_mint == pair.in_mint && p.out_mint == pair.out_mint));
        if pair.max_in.0 > 0 {
            require!(cfg.pairs.len() < 16, "too many pairs");
            cfg.pairs.push(pair);
        }
    }

    /// Owner or keeper: a Jupiter swap message, checked byte by byte, signed if allowed.
    pub(crate) fn sol_sign_swap(&mut self, message: String) -> Reply {
        let (pair, _) = self.sol_check(&message, None);
        match pair.market {
            None => self.sol_sign(&message, None),
            Some(m) => {
                let views = self.feed_views(&[m.sell.clone(), m.buy.clone()]);
                self.need_after = GAS_SIGN.as_tgas() + GAS_SIGNED.as_tgas() + crate::RELAY_TGAS;
                self.read(views, Read::SolPrices(message), GAS_CHECK)
            }
        }
    }

    pub(crate) fn on_sol_prices(&mut self, message: String) -> Reply {
        let (pair, _) = self.sol_check(&message, None);
        let m = pair.market.unwrap();
        let p = self.prices(&[m.sell.clone(), m.buy.clone()], 0);
        self.sol_sign(&message, Some(crate::oracle::market_per_in(&m, p[0], p[1], 1_000_000_000_000)))
    }

    /// The first allowed pair whose policy accepts the message (solguard), with its verdict.
    fn sol_check(&self, message: &str, market_e12: Option<u128>) -> (SolPair, solguard::Verdict) {
        let bytes = decode_message(message);
        let cfg = self.sol_cfg();
        let address = cfg.address.clone().unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known yet"));
        let authority = solguard::pubkey(&address);
        let mut why = String::from("no pair allowed");
        for pair in &cfg.pairs {
            let policy = solguard::SwapPolicy {
                authority,
                in_mint: solguard::pubkey(&pair.in_mint),
                out_mint: solguard::pubkey(&pair.out_mint),
                max_in: u64::try_from(pair.max_in.0).unwrap_or(u64::MAX),
                min_out_per_in_e12: pair.min_out_per_in_e12.0.max(if pair.market.is_some() { market_e12.unwrap_or(0) } else { 0 }),
                max_cu_price: cfg.max_cu_price,
                max_cu_limit: cfg.max_cu_limit,
            };
            match solguard::check_swap(&bytes, &policy) {
                Ok(v) => return (pair.clone(), v),
                Err(e) => why = format!("{e:?}"),
            }
        }
        env::panic_str(&format!("refused: {why}"))
    }

    fn sol_sign(&mut self, message: &str, market_e12: Option<u128>) -> Reply {
        let (pair, verdict) = self.sol_check(message, market_e12);
        let cfg = self.sol_cfg();
        let bytes = decode_message(message);
        // The amount counted is the one READ FROM THE MESSAGE.
        self.charge(&format!("sol:{}", pair.in_mint), verdict.in_amount as u128);
        let hash = hex::encode(env::sha256(&bytes));
        self.emit(
            "sol_sign_requested",
            &format!(r#""hash":"{hash}","in_mint":"{}","out_mint":"{}","in_amount":"{}","min_out":"{}","max_priority_fee_lamports":"{}""#,
                pair.in_mint, pair.out_mint, verdict.in_amount, verdict.min_out, verdict.max_priority_fee_lamports),
        );
        self.charge_signature(sol_sign_call(&cfg.signer, &bytes)).then("on_vault", near_sdk::borsh::to_vec(&Next::SolSigned(hash)).unwrap(), GAS_SIGNED).into()
    }

    /// The vault's `sign` came back: publish the signature (`sol_signed`) and return it.
    pub(crate) fn sol_emit_signed(&self, hash: &str) -> Option<String> {
        let sig = crate::result(0, 4096).and_then(|raw| crate::intents::signature_from(&raw)).map(hex::encode);
        match &sig {
            Some(s) => self.emit("sol_signed", &format!(r#""hash":"{hash}","signature":"{s}""#)),
            None => self.emit("sol_sign_failed", &format!(r#""hash":"{hash}""#)),
        }
        sig
    }

    pub(crate) fn on_sol_signed(&mut self, hash: String) -> Reply {
        let sig = self.sol_emit_signed(&hash);
        let ret = sig.map_or("null".into(), |s| format!(r#"{{"hash":"{hash}","signature":"{s}"}}"#));
        Answer::default().ret(&ret).into()
    }

    // ---------- templates (vault/src/soltemplates.rs) ----------

    pub(crate) fn sol_owner_wallet(&mut self, wallet: Option<String>) {
        if let Some(w) = &wallet {
            key(w);
        }
        let cfg = self.vs.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.owner_wallet = wallet;
    }

    pub(crate) fn sol_template(&mut self, template: SolTemplate) {
        template.to_guard(None).validate().unwrap_or_else(|e| env::panic_str(&e));
        for (i, f) in template.fields.iter().enumerate() {
            if let FieldJ::Market { market: m, .. } = &f.rule {
                require!(m.slippage_bps <= 5_000 && m.dec_in <= 36 && m.dec_out <= 36 && !m.sell.is_empty() && !m.buy.is_empty(),
                    format!("field {i}: market: two feeds, slippage <= 50%, decimals <= 36"));
            }
        }
        for s in &template.spends {
            require!(matches!(template.fields.get(s.field as usize).map(|f| &f.rule), Some(FieldJ::AtMost { .. })), "spends: an at_most field");
        }
        let cfg = self.vs.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.templates.retain(|t| t.name != template.name);
        require!(cfg.templates.len() < 32, "too many templates");
        cfg.templates.push(template);
    }

    pub(crate) fn sol_template_remove(&mut self, name: String) {
        if let Some(cfg) = self.vs.sol.as_mut() {
            cfg.templates.retain(|t| t.name != name);
        }
    }

    /// The owner's templates, checked against a message (every instruction must match one).
    /// `prices`: the market floors' (see `to_guard`); None checks the fixed floors alone.
    pub(crate) fn sol_matched(&self, bytes: &[u8], prices: Option<&[(String, u128)]>) -> Vec<solguard::template::Matched> {
        let cfg = self.sol_cfg();
        let address = cfg.address.clone().unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known yet"));
        let policy = solguard::template::TxPolicy {
            authority: key(&address),
            owner: cfg.owner_wallet.as_deref().map(key),
            max_cu_price: cfg.max_cu_price,
            max_cu_limit: cfg.max_cu_limit,
        };
        // A market template whose prices weren't read can't match (its instruction finds no template).
        let priced = |t: &&SolTemplate| prices.map_or(true, |p| t.market_feeds().iter().all(|f| p.iter().any(|(n, _)| n == f)));
        let guards: Vec<Template> = cfg.templates.iter().filter(priced).map(|t| t.to_guard(prices)).collect();
        solguard::template::check_tx(bytes, &guards, &policy).unwrap_or_else(|e| env::panic_str(&format!("refused: {e:?}")))
    }

    /// The feeds the market fields of these matched templates need, each once.
    pub(crate) fn sol_feeds(&self, names: &[&str]) -> Vec<String> {
        let cfg = self.sol_cfg();
        let mut out: Vec<String> = Vec::new();
        for t in cfg.templates.iter().filter(|t| names.contains(&t.name.as_str())) {
            for f in t.market_feeds() {
                if !out.contains(&f) {
                    out.push(f);
                }
            }
        }
        out
    }

    /// The feeds a message's templates need (matched against their fixed floors first).
    fn sol_tx_feeds(&self, bytes: &[u8]) -> Vec<String> {
        let matched = self.sol_matched(bytes, None);
        self.sol_feeds(&matched.iter().map(|m| m.template.as_str()).collect::<Vec<_>>())
    }

    /// Owner or keeper: a transaction made only of approved templates, signed. A template
    /// with a market field: the brain reads the prices first, then checks again (`on_sol_tx_prices`).
    pub(crate) fn sol_sign_tx(&mut self, message: String) -> Reply {
        let feeds = self.sol_tx_feeds(&decode_message(&message));
        if feeds.is_empty() {
            return self.sol_sign_tx_at(message, &[]);
        }
        let views = self.feed_views(&feeds);
        self.need_after = GAS_SIGN.as_tgas() + GAS_SIGNED.as_tgas() + crate::RELAY_TGAS;
        self.read(views, Read::SolTxPrices(message), GAS_CHECK)
    }

    pub(crate) fn on_sol_tx_prices(&mut self, message: String) -> Reply {
        let feeds = self.sol_tx_feeds(&decode_message(&message));
        let prices = self.named_prices(&feeds, 0);
        self.sol_sign_tx_at(message, &prices)
    }

    fn sol_sign_tx_at(&mut self, message: String, prices: &[(String, u128)]) -> Reply {
        let bytes = decode_message(&message);
        let cfg = self.sol_cfg();
        let matched = self.sol_matched(&bytes, Some(prices));
        // Spending is read from the message itself.
        for m in &matched {
            let t = cfg.templates.iter().find(|t| t.name == m.template).unwrap();
            for s in &t.spends {
                self.charge(&s.key, m.values[s.field as usize]);
            }
        }
        let names: Vec<String> = matched.iter().map(|m| format!(r#""{}""#, m.template)).collect();
        let hash = hex::encode(env::sha256(&bytes));
        self.emit("sol_sign_requested", &format!(r#""hash":"{hash}","templates":[{}]"#, names.join(",")));
        self.charge_signature(sol_sign_call(&cfg.signer, &bytes)).then("on_vault", near_sdk::borsh::to_vec(&Next::SolSigned(hash)).unwrap(), GAS_SIGNED).into()
    }
}
