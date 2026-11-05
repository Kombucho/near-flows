//! Program-derived addresses, to prove a token account belongs to the vault.
//!
//! address = sha256(seeds.. || bump || program_id || "ProgramDerivedAddress").
//! An associated token account is the PDA of [owner, token_program, mint]
//! under the Associated Token program. Nobody holds a private key for such an
//! address, and nobody can make sha256 land on an account of their own, so an
//! account that re-derives from the VAULT'S address is the vault's, whatever
//! the bump. (The real account uses the highest off-curve bump; a different
//! bump would point at an account that doesn't exist, and the swap would fail.)

use sha2::{Digest, Sha256};

use crate::{pubkey, Pubkey, ATA_PROGRAM};

fn pda(seeds: &[&[u8]], bump: u8, program: &Pubkey) -> Pubkey {
    let mut h = Sha256::new();
    for s in seeds {
        h.update(s);
    }
    h.update([bump]);
    h.update(program);
    h.update(b"ProgramDerivedAddress");
    h.finalize().into()
}

/// Does `target` derive from these seeds under `program`, with any bump? Returns the bump.
pub fn find_pda_with_any_bump(seeds: &[&[u8]], program: &Pubkey, target: &Pubkey) -> Option<u8> {
    (0..=255u8).rev().find(|&b| pda(seeds, b, program) == *target)
}

/// Is `account` an associated token account of `owner` for `mint` (any token program given)?
pub fn owns_ata(owner: &Pubkey, token_program: &Pubkey, mint: &Pubkey, account: &Pubkey) -> bool {
    find_pda_with_any_bump(&[owner, token_program, mint], &pubkey(ATA_PROGRAM), account).is_some()
}
