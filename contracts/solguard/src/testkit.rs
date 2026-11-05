//! Builders for Solana messages, for tests here and in the sandbox (feature "testkit").

use sha2::{Digest, Sha256};

use crate::*;

pub fn key(n: u8) -> Pubkey {
    [n; 32]
}

/// The bump-255 derivation, enough for tests (any bump is accepted).
pub fn derive(seeds: &[&[u8]], program: &Pubkey) -> Pubkey {
    let mut h = Sha256::new();
    for s in seeds {
        h.update(s);
    }
    h.update([255u8]);
    h.update(program);
    h.update(b"ProgramDerivedAddress");
    h.finalize().into()
}

pub fn ata_of(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    derive(&[owner, &pubkey(TOKEN_PROGRAM), mint], &pubkey(ATA_PROGRAM))
}

pub const VAULT: u8 = 7;
pub const USDC: u8 = 1;
pub const SOL: u8 = 2;

pub fn policy() -> SwapPolicy {
    SwapPolicy {
        authority: key(VAULT), in_mint: key(USDC), out_mint: key(SOL), max_in: 100_000_000,
        // at least 4 output units per input unit (scaled 1e12)
        min_out_per_in_e12: 4_000_000_000_000, max_cu_price: 100_000, max_cu_limit: 1_400_000,
    }
}

pub fn shortvec(n: usize, out: &mut Vec<u8>) {
    let mut v = n;
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// A message builder: accounts by key, instructions by (program, accounts, data).
pub struct Tx {
    pub keys: Vec<Pubkey>,
    pub ixs: Vec<(Pubkey, Vec<Pubkey>, Vec<u8>)>,
    pub signers: u8,
    pub v0_lookups: usize,
}

impl Tx {
    pub fn new(fee_payer: Pubkey) -> Tx {
        Tx { keys: vec![fee_payer], ixs: vec![], signers: 1, v0_lookups: 0 }
    }
    pub fn ix(mut self, program: Pubkey, accounts: Vec<Pubkey>, data: Vec<u8>) -> Tx {
        self.ixs.push((program, accounts, data));
        self
    }
    fn idx(&mut self, k: Pubkey) -> u8 {
        if let Some(i) = self.keys.iter().position(|x| *x == k) {
            return i as u8;
        }
        self.keys.push(k);
        (self.keys.len() - 1) as u8
    }
    pub fn bytes(mut self) -> Vec<u8> {
        let ixs: Vec<_> = std::mem::take(&mut self.ixs)
            .into_iter()
            .map(|(p, a, d)| (self.idx(p), a.into_iter().map(|k| self.idx(k)).collect::<Vec<_>>(), d))
            .collect();
        let mut out = vec![];
        if self.v0_lookups > 0 {
            out.push(0x80);
        }
        out.extend([self.signers, 0, 0]);
        shortvec(self.keys.len(), &mut out);
        for k in &self.keys {
            out.extend(k);
        }
        out.extend([9u8; 32]); // blockhash
        shortvec(ixs.len(), &mut out);
        for (p, a, d) in ixs {
            out.push(p);
            shortvec(a.len(), &mut out);
            out.extend(a);
            shortvec(d.len(), &mut out);
            out.extend(d);
        }
        if self.v0_lookups > 0 {
            shortvec(self.v0_lookups, &mut out);
            for _ in 0..self.v0_lookups {
                out.extend([3u8; 32]);
                shortvec(1, &mut out);
                out.push(0);
                shortvec(0, &mut out);
            }
        }
        out
    }
}

pub fn jup_data(name: &str, in_amount: u64, quoted_out: u64, slippage: u16, fee_bps: u8) -> Vec<u8> {
    let mut d = discriminator(name).to_vec();
    d.extend([0u8, 1, 0, 0, 0, 7, 100, 0, 1]); // id + a route plan (opaque here)
    d.extend(in_amount.to_le_bytes());
    d.extend(quoted_out.to_le_bytes());
    d.extend(slippage.to_le_bytes());
    d.push(fee_bps);
    d
}

/// shared_accounts_route accounts, with the user's accounts filled in.
pub fn shared_accounts(authority: Pubkey, src: Pubkey, dst: Pubkey, src_mint: Pubkey, dst_mint: Pubkey, fee: Pubkey) -> Vec<Pubkey> {
    vec![pubkey(TOKEN_PROGRAM), key(40), authority, src, key(41), key(42), dst, src_mint, dst_mint, fee, pubkey(JUPITER_V6), key(43), pubkey(JUPITER_V6), key(60), key(61)]
}

pub fn honest() -> Tx {
    let (v, usdc, sol) = (key(VAULT), key(USDC), key(SOL));
    Tx::new(v)
        .ix(pubkey(COMPUTE_BUDGET), vec![], [vec![2], 600_000u32.to_le_bytes().to_vec()].concat())
        .ix(pubkey(COMPUTE_BUDGET), vec![], [vec![3], 50_000u64.to_le_bytes().to_vec()].concat())
        .ix(pubkey(ATA_PROGRAM), vec![v, ata_of(&v, &sol), v, sol, pubkey(SYSTEM_PROGRAM), pubkey(TOKEN_PROGRAM)], vec![1])
        .ix(pubkey(JUPITER_V6), shared_accounts(v, ata_of(&v, &usdc), ata_of(&v, &sol), usdc, sol, pubkey(JUPITER_V6)),
            jup_data("shared_accounts_route", 10_000_000, 50_000_000, 50, 0))
}


/// An honest USDC -> SOL swap for any vault address and real-looking mints.
pub fn honest_for(vault: Pubkey, usdc: Pubkey, sol: Pubkey, in_amount: u64, quoted_out: u64) -> Vec<u8> {
    Tx::new(vault)
        .ix(pubkey(COMPUTE_BUDGET), vec![], [vec![2], 600_000u32.to_le_bytes().to_vec()].concat())
        .ix(pubkey(COMPUTE_BUDGET), vec![], [vec![3], 50_000u64.to_le_bytes().to_vec()].concat())
        .ix(pubkey(ATA_PROGRAM), vec![vault, ata_of(&vault, &sol), vault, sol, pubkey(SYSTEM_PROGRAM), pubkey(TOKEN_PROGRAM)], vec![1])
        .ix(pubkey(JUPITER_V6), shared_accounts(vault, ata_of(&vault, &usdc), ata_of(&vault, &sol), usdc, sol, pubkey(JUPITER_V6)),
            jup_data("shared_accounts_route", in_amount, quoted_out, 50, 0))
        .bytes()
}
