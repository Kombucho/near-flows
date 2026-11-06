//! Solana TEMPLATES: any program's instruction, checked byte by byte against
//! an owner-approved description of it (the Solana twin of the vault's EVM
//! templates). Jupiter's variable-length routes keep their own checker
//! (`check_swap`); everything with a fixed layout (pump.fun, Meteora DLMM,
//! Raydium...) is a template.
//!
//! A template says, for one instruction of one program:
//! - its data starts with these bytes (Anchor's 8-byte discriminator),
//! - then these fields, each with a type and a rule: fixed, any, at most
//!   (an amount), or at least `field[of] * num / den` (a minimum out),
//! - its accounts, one rule each: the vault itself, the vault's token
//!   account for the mint at position N (re-derived here, so it can't be
//!   someone else's) or for a fixed mint (wSOL when wrapping), a fixed address, the owner's wallet (who must also
//!   sign), a fresh key that signs (a new position), or any (pool and
//!   program accounts the protocol itself checks against each other),
//! - how many extra trailing accounts it may carry (protocol upgrades
//!   append some; they're never signers),
//! - and whether the owner's wallet must co-sign (LP opens: the owner approves the range).
//! There is no rule for "a token account the caller chooses": proceeds can
//! only land in the vault's own accounts.
//!
//! A transaction passes (`check_tx`) when the vault is the only fee payer,
//! every other signer is the owner's wallet or a fresh key a template asks
//! for, and every instruction is Compute Budget (capped), the creation of one
//! of the vault's own token accounts, or matches an allowed template.

use crate::message::{Instruction, Message};
use crate::{pda, pubkey, Pubkey, Refusal, ATA_PROGRAM, COMPUTE_BUDGET};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    U8,
    U16,
    U32,
    U64,
    U128,
    I32,
    I64,
    Bool,
    /// A fixed-size byte array.
    Bytes(u16),
}

impl Ty {
    pub fn len(&self) -> usize {
        match self {
            Ty::U8 | Ty::Bool => 1,
            Ty::U16 => 2,
            Ty::U32 | Ty::I32 => 4,
            Ty::U64 | Ty::I64 => 8,
            Ty::U128 => 16,
            Ty::Bytes(n) => *n as usize,
        }
    }
    fn unsigned(&self) -> bool {
        matches!(self, Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::U128)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldRule {
    /// Exactly these bytes (little-endian for numbers).
    Fixed(Vec<u8>),
    Any,
    /// An unsigned number, at most this.
    AtMost(u128),
    /// An unsigned number, at least `field[of] * num / den`.
    AtLeast { of: u8, num: u128, den: u128 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub ty: Ty,
    pub rule: FieldRule,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Acc {
    /// Anything: an account the program itself validates (pool, PDA, program id).
    Any,
    Fixed(Pubkey),
    /// The vault's own address.
    Vault,
    /// The vault's associated token account for the mint at account position
    /// `mint_at`, under the token program at position `program_at`.
    VaultAta { mint_at: u8, program_at: u8 },
    /// The vault's associated token account for a mint the instruction doesn't name (e.g. wrapping
    /// SOL: a System transfer into the vault's wSOL account, then sync_native and close on it).
    VaultAtaOf { mint: Pubkey, program: Pubkey },
    /// The owner's wallet: must sign this transaction too.
    Owner,
    /// A fresh key that signs (e.g. a new Meteora position). Never the vault, never reused as a token account.
    NewSigner,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    pub program: Pubkey,
    pub prefix: Vec<u8>,
    pub fields: Vec<Field>,
    pub accounts: Vec<Acc>,
    /// Extra trailing accounts allowed (never signers).
    pub extra_accounts: u8,
    /// The owner's wallet must sign any transaction with this instruction
    /// (e.g. opening an LP position: the owner sees and approves the range).
    pub needs_owner: bool,
}

/// What a passing instruction said, for limits and reporting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Matched {
    pub template: String,
    /// Each field's value as a number (unsigned fields; 0 for others).
    pub values: Vec<u128>,
}

#[derive(Clone, Debug)]
pub struct TxPolicy {
    pub authority: Pubkey,
    /// The owner's Solana wallet, if they co-sign some actions.
    pub owner: Option<Pubkey>,
    pub max_cu_price: u64,
    pub max_cu_limit: u32,
}

impl Template {
    /// Check the template itself (when the owner saves it).
    pub fn validate(&self) -> Result<(), String> {
        if self.name.is_empty() || self.name.len() > 40 {
            return Err("name: 1 to 40 characters".into());
        }
        if self.fields.len() > 24 || self.accounts.len() > 32 || self.prefix.len() > 16 {
            return Err("too many fields or accounts".into());
        }
        for (i, f) in self.fields.iter().enumerate() {
            match &f.rule {
                FieldRule::Fixed(b) if b.len() != f.ty.len() => return Err(format!("field {i}: fixed value of the wrong size")),
                FieldRule::AtMost(_) if !f.ty.unsigned() => return Err(format!("field {i}: at_most needs an unsigned number")),
                FieldRule::AtLeast { of, den, .. } => {
                    if !f.ty.unsigned() || *den == 0 {
                        return Err(format!("field {i}: at_least needs an unsigned number and den > 0"));
                    }
                    match self.fields.get(*of as usize) {
                        Some(Field { rule: FieldRule::AtMost(_), .. }) => {}
                        _ => return Err(format!("field {i}: at_least is relative to an at_most amount")),
                    }
                }
                _ => {}
            }
        }
        for (i, a) in self.accounts.iter().enumerate() {
            if let Acc::VaultAta { mint_at, program_at } = a {
                if *mint_at as usize >= self.accounts.len() || *program_at as usize >= self.accounts.len() {
                    return Err(format!("account {i}: mint/program position out of range"));
                }
            }
        }
        Ok(())
    }

    /// Does instruction `ix` of message `m` match? The field values if so.
    fn check(&self, m: &Message, ix: &Instruction, p: &TxPolicy, signers: &[Pubkey]) -> Result<Vec<u128>, String> {
        let data = &ix.data;
        if !data.starts_with(&self.prefix) {
            return Err("another instruction".into());
        }
        let want: usize = self.prefix.len() + self.fields.iter().map(|f| f.ty.len()).sum::<usize>();
        if data.len() != want {
            return Err(format!("data is {} bytes, the template says {want}", data.len()));
        }
        let mut at = self.prefix.len();
        let mut raw: Vec<&[u8]> = Vec::new();
        let mut values: Vec<u128> = Vec::new();
        for f in &self.fields {
            let b = &data[at..at + f.ty.len()];
            at += f.ty.len();
            raw.push(b);
            values.push(if f.ty.unsigned() { le(b) } else { 0 });
        }
        for (i, f) in self.fields.iter().enumerate() {
            match &f.rule {
                FieldRule::Fixed(v) if raw[i] != v.as_slice() => return Err(format!("field {i}: not the fixed value")),
                FieldRule::AtMost(max) if values[i] > *max => return Err(format!("field {i}: {} over the cap {max}", values[i])),
                FieldRule::AtLeast { of, num, den } => {
                    let floor = mul_div(values[*of as usize], *num, *den);
                    if values[i] < floor {
                        return Err(format!("field {i}: {} under the floor {floor}", values[i]));
                    }
                }
                _ => {}
            }
        }
        let n = self.accounts.len();
        if ix.accounts.len() < n || ix.accounts.len() > n + self.extra_accounts as usize {
            return Err(format!("{} accounts, the template says {n} (+{} extra)", ix.accounts.len(), self.extra_accounts));
        }
        let key = |i: usize| m.key(ix.accounts[i]).copied().ok_or_else(|| "bad account index".to_string());
        for (i, rule) in self.accounts.iter().enumerate() {
            let k = key(i)?;
            let ok = match rule {
                Acc::Any => true,
                Acc::Fixed(f) => k == *f,
                Acc::Vault => k == p.authority,
                Acc::VaultAta { mint_at, program_at } => {
                    let (mint, prog) = (key(*mint_at as usize)?, key(*program_at as usize)?);
                    pda::owns_ata(&p.authority, &prog, &mint, &k)
                }
                Acc::VaultAtaOf { mint, program } => pda::owns_ata(&p.authority, program, mint, &k),
                Acc::Owner => Some(k) == p.owner && signers.contains(&k),
                Acc::NewSigner => k != p.authority && Some(k) != p.owner && signers.contains(&k),
            };
            if !ok {
                return Err(format!("account {i}: not allowed ({rule:?})"));
            }
        }
        // Trailing extras: anything the program validates, but never a signer.
        for i in n..ix.accounts.len() {
            if signers.contains(&key(i)?) {
                return Err(format!("extra account {i} is a signer"));
            }
        }
        Ok(values)
    }

    /// The keys this template needs as co-signers in `ix` (owner, new signers).
    fn cosigners(&self, m: &Message, ix: &Instruction) -> Vec<Pubkey> {
        self.accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| matches!(a, Acc::Owner | Acc::NewSigner))
            .filter_map(|(i, _)| ix.accounts.get(i).and_then(|&k| m.key(k)).copied())
            .collect()
    }
}

/// Check a whole transaction message against the allowed templates.
pub fn check_tx(message_bytes: &[u8], templates: &[Template], p: &TxPolicy) -> Result<Vec<Matched>, Refusal> {
    let m = Message::parse(message_bytes).map_err(Refusal::Parse)?;
    if m.has_lookup_tables {
        return Err(Refusal::LookupTables);
    }
    let nsig = m.num_required_signatures as usize;
    if nsig == 0 || m.account_keys.first() != Some(&p.authority) {
        return Err(Refusal::FeePayer);
    }
    let signers: Vec<Pubkey> = m.account_keys.iter().take(nsig).copied().collect();
    let (cb, ata_prog) = (pubkey(COMPUTE_BUDGET), pubkey(ATA_PROGRAM));
    let (cu_price, cu_limit) = crate::compute_budget(&m)?;
    let mut matched = Vec::new();
    let mut wanted: Vec<Pubkey> = vec![p.authority];
    for ix in &m.instructions {
        let prog = *m.key(ix.program_index).ok_or(Refusal::Program("bad index".into()))?;
        if prog == cb {
            continue;
        }
        if prog == ata_prog {
            crate::check_ata_create_for(&m, ix, &p.authority)?;
            continue;
        }
        let mut why = format!("no template for program {}", bs58::encode(prog).into_string());
        let mut hit = None;
        for t in templates.iter().filter(|t| t.program == prog) {
            match t.check(&m, ix, p, &signers) {
                Ok(values) => {
                    hit = Some((t, values));
                    break;
                }
                // Report the template that was meant (its discriminator matched), not the last one tried.
                Err(e) if ix.data.starts_with(&t.prefix) || !why.contains(": ") => why = format!("{}: {e}", t.name),
                Err(_) => {}
            }
        }
        let (t, values) = hit.ok_or(Refusal::Program(why))?;
        if t.needs_owner && !p.owner.is_some_and(|o| signers.contains(&o)) {
            return Err(Refusal::Signers);
        }
        if t.needs_owner {
            wanted.extend(p.owner);
        }
        wanted.extend(t.cosigners(&m, ix));
        matched.push(Matched { template: t.name.clone(), values });
    }
    if matched.is_empty() {
        return Err(Refusal::NoSwap);
    }
    // Every signer must be one the templates asked for: the vault, the owner, fresh position keys.
    if signers.iter().any(|s| !wanted.contains(s)) {
        return Err(Refusal::Signers);
    }
    if cu_price > p.max_cu_price || cu_limit > p.max_cu_limit {
        return Err(Refusal::ComputeBudget(format!("price {cu_price} / limit {cu_limit} over the cap")));
    }
    Ok(matched)
}

fn le(b: &[u8]) -> u128 {
    let mut x = [0u8; 16];
    x[..b.len().min(16)].copy_from_slice(&b[..b.len().min(16)]);
    u128::from_le_bytes(x)
}

fn mul_div(a: u128, b: u128, d: u128) -> u128 {
    // a * b / d without overflow for the sizes Solana amounts have (u64 x u64).
    if let Some(x) = a.checked_mul(b) {
        return x / d;
    }
    (a / d).saturating_mul(b).saturating_add((a % d).saturating_mul(b) / d)
}
