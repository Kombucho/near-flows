//! Solana templates in the vault: the owner approves instruction templates
//! (solguard::template: pump.fun, Meteora DLMM, Raydium...), and the keeper may
//! have any transaction signed that is made only of those instructions (plus
//! capped Compute Budget and the vault's own token accounts). Amounts the
//! templates mark as spending are read from the message and counted against
//! the vault's spending limits. Templates that open LP positions require the
//! owner's own Solana wallet to co-sign: the vault adds its signature, the
//! owner's app adds theirs.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, NearToken, Promise};
use solguard::template::{Acc, Field, FieldRule, Template, Ty};

use crate::sol::SOL_PATH;
use crate::{Vault, VaultExt};

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

impl SolTemplate {
    pub fn to_guard(&self) -> Template {
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

#[near]
impl Vault {
    /// Owner: the owner's own Solana wallet (co-signs LP opens and other `needs_owner` templates).
    pub fn sol_owner_wallet(&mut self, wallet: Option<String>) {
        self.assert_owner();
        if let Some(w) = &wallet {
            key(w);
        }
        let cfg = self.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.owner_wallet = wallet;
    }

    /// Owner: add a Solana template, or replace the one with the same name.
    pub fn sol_template(&mut self, template: SolTemplate) {
        self.assert_owner();
        template.to_guard().validate().unwrap_or_else(|e| env::panic_str(&e));
        for s in &template.spends {
            require!(
                matches!(template.fields.get(s.field as usize).map(|f| &f.rule), Some(FieldJ::AtMost { .. })),
                "spends: an at_most field"
            );
        }
        let cfg = self.sol.as_mut().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        cfg.templates.retain(|t| t.name != template.name);
        require!(cfg.templates.len() < 32, "too many templates");
        cfg.templates.push(template);
    }

    pub fn sol_template_remove(&mut self, name: String) {
        self.assert_owner();
        if let Some(cfg) = self.sol.as_mut() {
            cfg.templates.retain(|t| t.name != name);
        }
    }

    /// Owner or keeper: have a Solana transaction made of approved templates signed.
    /// `message`: the message bytes, base64. Emits `sol_signed`; a `needs_owner`
    /// transaction still needs the owner's own signature before it can land.
    pub fn sol_sign_tx(&mut self, message: String) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        let bytes = near_sdk::base64::Engine::decode(&near_sdk::base64::engine::general_purpose::STANDARD, &message).unwrap_or_else(|_| env::panic_str("message: base64"));
        require!(bytes.len() <= 1232, "a Solana message fits in 1232 bytes");
        let cfg = self.sol.clone().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        let address = cfg.address.clone().unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known yet"));
        let policy = solguard::template::TxPolicy {
            authority: key(&address),
            owner: cfg.owner_wallet.as_deref().map(key),
            max_cu_price: cfg.max_cu_price,
            max_cu_limit: cfg.max_cu_limit,
        };
        let guards: Vec<Template> = cfg.templates.iter().map(SolTemplate::to_guard).collect();
        let matched = solguard::template::check_tx(&bytes, &guards, &policy).unwrap_or_else(|e| env::panic_str(&format!("refused: {e:?}")));
        // Spending is read from the message itself.
        for m in &matched {
            let t = cfg.templates.iter().find(|t| t.name == m.template).unwrap();
            for s in &t.spends {
                self.charge(&s.key, m.values[s.field as usize]);
            }
        }
        let names: Vec<String> = matched.iter().map(|m| format!(r#""{}""#, m.template)).collect();
        let hash = hex::encode(env::sha256(&bytes));
        self.emit("sol_sign_requested", &format!(r#"{{"hash":"{hash}","templates":[{}]}}"#, names.join(",")));
        let args = near_sdk::serde_json::json!({"request": {"path": SOL_PATH, "payload_v2": {"Eddsa": hex::encode(&bytes)}, "domain_id": crate::sol::EDDSA_DOMAIN}});
        Promise::new(cfg.signer)
            .function_call("sign".to_string(), args.to_string().into_bytes(), NearToken::from_yoctonear(1), crate::sol::GAS_SIGN)
            .then(Self::ext(env::current_account_id()).with_static_gas(crate::sol::GAS_CB).on_sol_signed(hash))
    }
}
