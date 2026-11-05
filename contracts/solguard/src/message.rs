//! Solana transaction messages, legacy and v0 (the bytes a signer signs).
//!
//!   [0x80 | version]?            v0 only
//!   header: 3 bytes              required signatures, readonly signed, readonly unsigned
//!   account keys: compact-u16 n, n x 32 bytes
//!   recent blockhash: 32 bytes
//!   instructions: compact-u16 n, each: program index u8, compact-u16 account indexes, compact-u16 data
//!   v0 only: address table lookups: compact-u16 n, each: 32-byte table, compact-u16 writable, compact-u16 readonly

use crate::Pubkey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParseError {
    Short,
    Version(u8),
    Trailing,
}

#[derive(Clone, Debug)]
pub struct Instruction {
    pub program_index: u8,
    pub accounts: Vec<u8>,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Message {
    pub version: Option<u8>,
    pub num_required_signatures: u8,
    pub account_keys: Vec<Pubkey>,
    pub recent_blockhash: Pubkey,
    pub instructions: Vec<Instruction>,
    pub has_lookup_tables: bool,
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, ParseError> {
        let v = *self.b.get(self.i).ok_or(ParseError::Short)?;
        self.i += 1;
        Ok(v)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        let s = self.b.get(self.i..self.i + n).ok_or(ParseError::Short)?;
        self.i += n;
        Ok(s)
    }
    /// Solana's "shortvec": 7 bits per byte, at most 3 bytes.
    fn compact_u16(&mut self) -> Result<usize, ParseError> {
        let mut v = 0usize;
        for k in 0..3 {
            let b = self.u8()?;
            v |= ((b & 0x7f) as usize) << (7 * k);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(ParseError::Short)
    }
    fn key(&mut self) -> Result<Pubkey, ParseError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
}

impl Message {
    pub fn parse(bytes: &[u8]) -> Result<Message, ParseError> {
        let mut r = Reader { b: bytes, i: 0 };
        let first = *bytes.first().ok_or(ParseError::Short)?;
        let version = if first & 0x80 != 0 {
            r.u8()?;
            let v = first & 0x7f;
            if v != 0 {
                return Err(ParseError::Version(v));
            }
            Some(0)
        } else {
            None
        };
        let num_required_signatures = r.u8()?;
        let _ro_signed = r.u8()?;
        let _ro_unsigned = r.u8()?;
        let n = r.compact_u16()?;
        let account_keys = (0..n).map(|_| r.key()).collect::<Result<Vec<_>, _>>()?;
        let recent_blockhash = r.key()?;
        let n = r.compact_u16()?;
        let mut instructions = Vec::with_capacity(n);
        for _ in 0..n {
            let program_index = r.u8()?;
            let na = r.compact_u16()?;
            let accounts = r.take(na)?.to_vec();
            let nd = r.compact_u16()?;
            let data = r.take(nd)?.to_vec();
            instructions.push(Instruction { program_index, accounts, data });
        }
        let mut has_lookup_tables = false;
        if version.is_some() {
            let n = r.compact_u16()?;
            for _ in 0..n {
                r.key()?;
                let w = r.compact_u16()?;
                r.take(w)?;
                let ro = r.compact_u16()?;
                r.take(ro)?;
            }
            has_lookup_tables = n > 0;
        }
        if r.i != bytes.len() {
            return Err(ParseError::Trailing);
        }
        Ok(Message { version, num_required_signatures, account_keys, recent_blockhash, instructions, has_lookup_tables })
    }

    /// The account at `index` (only static keys: lookup tables are refused).
    pub fn key(&self, index: u8) -> Option<&Pubkey> {
        self.account_keys.get(index as usize)
    }
}

/// The message inside a wire transaction: [compact-u16 n][n x 64-byte signatures][message].
pub fn message_of_transaction(tx: &[u8]) -> Result<&[u8], ParseError> {
    let mut r = Reader { b: tx, i: 0 };
    let n = r.compact_u16()?;
    r.take(64 * n)?;
    Ok(&tx[r.i..])
}
