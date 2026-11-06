//! SOLGUARD: the vault reads a Solana transaction before it signs it.
//!
//! A Jupiter swap is built off chain (Jupiter's API picks the route). The
//! vault can't build it itself, so instead it checks every byte of what it is
//! asked to sign, and refuses anything outside the owner's policy. The helper
//! that fetches the route and broadcasts the signed transaction can then only
//! do one thing: get a swap signed that the policy allows. It cannot make the
//! vault sign a transfer, an approval, a fee to someone else, or a swap whose
//! proceeds go anywhere but the vault's own token account.
//!
//! What is checked (`check_swap`):
//! - legacy or v0 message, and NO address lookup tables (every account visible);
//! - exactly one signer, the vault's own Solana address, which also pays fees;
//! - only three programs: Compute Budget (price and units capped, so fees are
//!   bounded), Associated Token (create the vault's own token account only),
//!   and Jupiter v6 (exactly one swap instruction);
//! - the swap: the vault is the authority; source and destination are the
//!   vault's own token accounts for the allowed mints (re-derived here from
//!   the vault's address, so they can't belong to anyone else); no platform
//!   fee; amount in within the limit; and the least it can receive after
//!   slippage is at least the owner's floor.

mod message;
mod pda;
pub mod template;

pub use message::{Instruction, Message, ParseError};
pub use message::message_of_transaction;
pub use pda::{find_pda_with_any_bump, owns_ata};

pub type Pubkey = [u8; 32];

pub fn pubkey(b58: &str) -> Pubkey {
    let v = bs58::decode(b58).into_vec().expect("base58");
    v.try_into().expect("32 bytes")
}

pub const JUPITER_V6: &str = "JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4";
pub const COMPUTE_BUDGET: &str = "ComputeBudget111111111111111111111111111111";
pub const ATA_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";

/// Anchor's instruction discriminator: sha256("global:<name>")[..8].
pub fn discriminator(name: &str) -> [u8; 8] {
    use sha2::{Digest, Sha256};
    let h = Sha256::digest(format!("global:{name}").as_bytes());
    h[..8].try_into().unwrap()
}

/// What the owner allows for one swap.
#[derive(Clone, Debug)]
pub struct SwapPolicy {
    /// The vault's own Solana address (its Chain Signatures-derived key).
    pub authority: Pubkey,
    pub in_mint: Pubkey,
    pub out_mint: Pubkey,
    /// Most of the input token one swap may spend (in its base units).
    pub max_in: u64,
    /// Least output per input, scaled 1e12: min_out >= in_amount * this / 1e12.
    pub min_out_per_in_e12: u128,
    /// Fee caps: priority price (micro-lamports per compute unit) and units.
    pub max_cu_price: u64,
    pub max_cu_limit: u32,
}

/// A swap the policy allows, as read from the message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub in_amount: u64,
    pub quoted_out: u64,
    pub slippage_bps: u16,
    /// The least the vault can receive (quoted out minus slippage).
    pub min_out: u64,
    /// Upper bound on the priority fee this transaction can cost, in lamports.
    pub max_priority_fee_lamports: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    Parse(ParseError),
    LookupTables,
    Signers,
    FeePayer,
    Program(String),
    ComputeBudget(String),
    AtaCreate(String),
    Swap(String),
    NoSwap,
    TwoSwaps,
}

/// Instruction layouts of Jupiter v6 (from its IDL). Index = position in the
/// instruction's account list.
struct Layout {
    authority: usize,
    source: usize,
    /// Where the user's output lands.
    destination: usize,
    /// For `route`: an optional second destination that overrides `destination`.
    alt_destination: Option<usize>,
    source_mint: Option<usize>,
    destination_mint: usize,
    platform_fee: usize,
    min_accounts: usize,
}

const ROUTE: Layout = Layout { authority: 1, source: 2, destination: 3, alt_destination: Some(4), source_mint: None, destination_mint: 5, platform_fee: 6, min_accounts: 9 };
const SHARED_ROUTE: Layout = Layout { authority: 2, source: 3, destination: 6, alt_destination: None, source_mint: Some(7), destination_mint: 8, platform_fee: 9, min_accounts: 13 };

pub fn check_swap(message_bytes: &[u8], p: &SwapPolicy) -> Result<Verdict, Refusal> {
    let m = Message::parse(message_bytes).map_err(Refusal::Parse)?;
    if m.has_lookup_tables {
        return Err(Refusal::LookupTables);
    }
    if m.num_required_signatures != 1 {
        return Err(Refusal::Signers);
    }
    if m.account_keys.first() != Some(&p.authority) {
        return Err(Refusal::FeePayer);
    }
    let (jup, cb, ata_prog) = (pubkey(JUPITER_V6), pubkey(COMPUTE_BUDGET), pubkey(ATA_PROGRAM));
    let (mut cu_price, mut cu_limit) = (0u64, 200_000u32);
    let mut verdict = None;
    for ix in &m.instructions {
        let prog = *m.key(ix.program_index).ok_or(Refusal::Program("bad index".into()))?;
        if prog == cb {
            match ix.data.first() {
                Some(2) if ix.data.len() == 5 => cu_limit = u32::from_le_bytes(ix.data[1..5].try_into().unwrap()),
                Some(3) if ix.data.len() == 9 => cu_price = u64::from_le_bytes(ix.data[1..9].try_into().unwrap()),
                _ => return Err(Refusal::ComputeBudget("only unit limit and unit price".into())),
            }
        } else if prog == ata_prog {
            check_ata_create(&m, ix, p)?;
        } else if prog == jup {
            if verdict.is_some() {
                return Err(Refusal::TwoSwaps);
            }
            verdict = Some(check_jupiter(&m, ix, p)?);
        } else {
            return Err(Refusal::Program(bs58::encode(prog).into_string()));
        }
    }
    if cu_price > p.max_cu_price || cu_limit > p.max_cu_limit {
        return Err(Refusal::ComputeBudget(format!("price {cu_price} / limit {cu_limit} over the cap")));
    }
    let mut v = verdict.ok_or(Refusal::NoSwap)?;
    v.max_priority_fee_lamports = ((cu_price as u128 * cu_limit as u128).div_ceil(1_000_000)) as u64;
    Ok(v)
}

/// Creating a token account is allowed only for the vault itself, paid by the vault.
fn check_ata_create(m: &Message, ix: &Instruction, p: &SwapPolicy) -> Result<(), Refusal> {
    // Create = [] or [0]; CreateIdempotent = [1]. Anything else (e.g. RecoverNested) is refused.
    if !(ix.data.is_empty() || ix.data == [0] || ix.data == [1]) {
        return Err(Refusal::AtaCreate("only create".into()));
    }
    let acc = |i: usize| ix.accounts.get(i).and_then(|&k| m.key(k)).copied().ok_or(Refusal::AtaCreate("missing account".into()));
    let (payer, account, owner, mint, token_prog) = (acc(0)?, acc(1)?, acc(2)?, acc(3)?, acc(5)?);
    if payer != p.authority || owner != p.authority {
        return Err(Refusal::AtaCreate("only for the vault, paid by the vault".into()));
    }
    if mint != p.in_mint && mint != p.out_mint {
        return Err(Refusal::AtaCreate("unexpected mint".into()));
    }
    if !owns_ata(&p.authority, &token_prog, &mint, &account) {
        return Err(Refusal::AtaCreate("not the vault's own token account".into()));
    }
    Ok(())
}

/// Creating one of the vault's own token accounts (any mint), paid by the vault:
/// templates may need one for a coin the vault never held (e.g. a fresh memecoin).
pub(crate) fn check_ata_create_for(m: &Message, ix: &Instruction, authority: &Pubkey) -> Result<(), Refusal> {
    if !(ix.data.is_empty() || ix.data == [0] || ix.data == [1]) {
        return Err(Refusal::AtaCreate("only create".into()));
    }
    let acc = |i: usize| ix.accounts.get(i).and_then(|&k| m.key(k)).copied().ok_or(Refusal::AtaCreate("missing account".into()));
    let (payer, account, owner, mint, token_prog) = (acc(0)?, acc(1)?, acc(2)?, acc(3)?, acc(5)?);
    if payer != *authority || owner != *authority || !owns_ata(authority, &token_prog, &mint, &account) {
        return Err(Refusal::AtaCreate("only the vault's own token account, paid by the vault".into()));
    }
    Ok(())
}

fn check_jupiter(m: &Message, ix: &Instruction, p: &SwapPolicy) -> Result<Verdict, Refusal> {
    let bad = |s: &str| Refusal::Swap(s.into());
    if ix.data.len() < 8 + 19 {
        return Err(bad("too short"));
    }
    let disc: [u8; 8] = ix.data[..8].try_into().unwrap();
    let layout = if disc == discriminator("route") {
        &ROUTE
    } else if disc == discriminator("shared_accounts_route") {
        &SHARED_ROUTE
    } else {
        return Err(bad("only route and shared_accounts_route (exact-in swaps)"));
    };
    if ix.accounts.len() < layout.min_accounts {
        return Err(bad("missing accounts"));
    }
    let acc = |i: usize| m.key(ix.accounts[i]).copied().ok_or(bad("bad account index"));
    let jup = pubkey(JUPITER_V6);
    if acc(layout.authority)? != p.authority {
        return Err(bad("the vault must be the swap's authority"));
    }
    let token_prog = acc(0)?;
    if token_prog != pubkey(TOKEN_PROGRAM) && token_prog != pubkey(TOKEN_2022) {
        return Err(bad("unknown token program"));
    }
    let own = |mint: &Pubkey, account: Pubkey| {
        [pubkey(TOKEN_PROGRAM), pubkey(TOKEN_2022)].iter().any(|tp| owns_ata(&p.authority, tp, mint, &account))
    };
    if !own(&p.in_mint, acc(layout.source)?) {
        return Err(bad("source must be the vault's own account for the input mint"));
    }
    if !own(&p.out_mint, acc(layout.destination)?) {
        return Err(bad("destination must be the vault's own account for the output mint"));
    }
    if let Some(i) = layout.alt_destination {
        let d = acc(i)?;
        if d != jup && !own(&p.out_mint, d) {
            return Err(bad("proceeds would go elsewhere"));
        }
    }
    if let Some(i) = layout.source_mint {
        if acc(i)? != p.in_mint {
            return Err(bad("wrong input mint"));
        }
    }
    if acc(layout.destination_mint)? != p.out_mint {
        return Err(bad("wrong output mint"));
    }
    if acc(layout.platform_fee)? != jup {
        return Err(bad("no platform fee account allowed"));
    }
    // The last 19 bytes: in_amount u64, quoted_out_amount u64, slippage_bps u16, platform_fee_bps u8.
    let t = &ix.data[ix.data.len() - 19..];
    let in_amount = u64::from_le_bytes(t[0..8].try_into().unwrap());
    let quoted_out = u64::from_le_bytes(t[8..16].try_into().unwrap());
    let slippage_bps = u16::from_le_bytes(t[16..18].try_into().unwrap());
    let fee_bps = t[18];
    if fee_bps != 0 {
        return Err(bad("no platform fee"));
    }
    if in_amount == 0 || in_amount > p.max_in {
        return Err(bad("amount in over the limit"));
    }
    if slippage_bps > 10_000 {
        return Err(bad("bad slippage"));
    }
    let min_out = (quoted_out as u128 * (10_000 - slippage_bps as u128) / 10_000) as u64;
    let floor = in_amount as u128 * p.min_out_per_in_e12 / 1_000_000_000_000;
    if (min_out as u128) < floor {
        return Err(bad("the least it could receive is under the owner's floor"));
    }
    Ok(Verdict { in_amount, quoted_out, slippage_bps, min_out, max_priority_fee_lamports: 0 })
}

#[cfg(any(test, feature = "testkit"))]
pub mod testkit;
#[cfg(test)]
mod tests;
