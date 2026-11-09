//! Wormhole on NEAR without Wormhole's NEAR contract: check guardian signatures
//! ourselves (secp256k1 ecrecover, as every Wormhole core contract does) on
//! - Query responses (Wormhole Queries / CCQ): guardians each run the same read
//!   on another chain (here: a Solana account at a program address) and sign
//!   keccak256("query_response_0000000000000000000|" || keccak256(response)).
//!   A quorum (2/3 + 1) of the current guardian set makes it a fact.
//! - Guardian set upgrade VAAs: the set signs its successor, so a board that
//!   starts from one known set can follow Wormhole's rotations with no owner.
//!
//! Formats (all big-endian), from wormhole-solidity-sdk QueryResponse.sol and
//! the core contracts; checked against real mainnet responses and VAAs in the tests.

use near_sdk::env;

pub const RESPONSE_PREFIX: &[u8] = b"query_response_0000000000000000000|";
pub const SOLANA: u16 = 1;
/// eth_call at a block the requester names ("latest" or a number). Types 2
/// (by timestamp) and 3 (with finality) aren't taken: finality on an L2 is
/// minutes to hours old, too stale for a price, and the board checks the
/// block's own time instead.
pub const QT_ETH_CALL: u8 = 1;
pub const QT_SOL_PDA: u8 = 5;

/// Reads a byte string front to back; every read is bounds-checked.
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], &'static str> {
        let end = self.at.checked_add(n).filter(|e| *e <= self.b.len()).ok_or("truncated")?;
        let s = &self.b[self.at..end];
        self.at = end;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, &'static str> {
        Ok(self.bytes(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, &'static str> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, &'static str> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, &'static str> {
        Ok(u64::from_be_bytes(self.bytes(8)?.try_into().unwrap()))
    }
    pub fn b32(&mut self) -> Result<[u8; 32], &'static str> {
        Ok(self.bytes(32)?.try_into().unwrap())
    }
    /// A u32-length-prefixed byte string.
    pub fn var(&mut self) -> Result<&'a [u8], &'static str> {
        let n = self.u32()? as usize;
        self.bytes(n)
    }
    pub fn done(&self) -> Result<(), &'static str> {
        if self.at == self.b.len() { Ok(()) } else { Err("trailing bytes") }
    }
}

/// One chain's query and its answer, still encoded.
#[derive(Debug, Clone, PartialEq)]
pub struct PerChain {
    pub chain: u16,
    pub kind: u8,
    pub request: Vec<u8>,
    pub response: Vec<u8>,
}

/// Split a query response into its (request, response) pairs. The response
/// repeats the whole request, so what was asked is signed along with the answer.
pub fn parse_response(b: &[u8]) -> Result<Vec<PerChain>, &'static str> {
    let mut r = Reader::new(b);
    if r.u8()? != 1 {
        return Err("response version");
    }
    let sender = r.u16()?;
    // Off-chain requests (sender chain 0) carry the requester's 65-byte signature, on-chain ones a VAA hash.
    r.bytes(if sender == 0 { 65 } else { 32 })?;
    let mut q = Reader::new(r.var()?);
    if q.u8()? != 1 {
        return Err("request version");
    }
    q.u32()?; // nonce
    let n = q.u8()?;
    if n == 0 || r.u8()? != n {
        return Err("query count");
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (chain, kind) = (q.u16()?, q.u8()?);
        let request = q.var()?.to_vec();
        if r.u16()? != chain || r.u8()? != kind {
            return Err("response doesn't match its request");
        }
        out.push(PerChain { chain, kind, request, response: r.var()?.to_vec() });
    }
    q.done()?;
    r.done()?;
    Ok(out)
}

/// One Solana account found at a program-derived address.
#[derive(Debug, Clone, PartialEq)]
pub struct PdaAccount {
    pub program: [u8; 32],
    pub seeds: Vec<Vec<u8>>,
    pub account: [u8; 32],
    pub bump: u8,
    pub lamports: u64,
    pub owner: [u8; 32],
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SolPda {
    pub commitment: Vec<u8>,
    pub slice: (u64, u64),
    pub slot: u64,
    pub block_time_us: u64,
    pub accounts: Vec<PdaAccount>,
}

/// A sol_pda query (type 5): the guardians derived each address from (program,
/// seeds) themselves, so the account IS the PDA for those seeds.
pub fn parse_sol_pda(pc: &PerChain) -> Result<SolPda, &'static str> {
    if pc.chain != SOLANA || pc.kind != QT_SOL_PDA {
        return Err("not a Solana PDA query");
    }
    let (mut q, mut r) = (Reader::new(&pc.request), Reader::new(&pc.response));
    let commitment = q.var()?.to_vec();
    q.u64()?; // min context slot
    let slice = (q.u64()?, q.u64()?);
    let n = q.u8()?;
    let (slot, block_time_us) = (r.u64()?, r.u64()?);
    r.b32()?; // block hash
    if r.u8()? != n {
        return Err("result count");
    }
    let mut accounts = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let program = q.b32()?;
        let ns = q.u8()?;
        let mut seeds = Vec::with_capacity(ns as usize);
        for _ in 0..ns {
            seeds.push(q.var()?.to_vec());
        }
        let account = r.b32()?;
        let bump = r.u8()?;
        let lamports = r.u64()?;
        r.u64()?; // rent epoch
        r.u8()?; // executable
        let owner = r.b32()?;
        let data = r.var()?.to_vec();
        accounts.push(PdaAccount { program, seeds, account, bump, lamports, owner, data });
    }
    q.done()?;
    r.done()?;
    Ok(SolPda { commitment, slice, slot, block_time_us, accounts })
}

/// One contract call the guardians made, and what it returned.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub to: [u8; 20],
    pub data: Vec<u8>,
    pub result: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EthCall {
    pub chain: u16,
    /// What the requester asked for: "latest" or a hex block number (ASCII).
    pub block_id: Vec<u8>,
    pub block_num: u64,
    pub block_hash: [u8; 32],
    pub block_time_us: u64,
    pub calls: Vec<Call>,
}

/// An eth_call query (type 1) on an EVM chain: every guardian ran the same
/// calls at the same block and signed the results, with the block's number,
/// hash and time. Layout from wormhole-solidity-sdk QueryResponse.sol
/// (parseEthCallQueryResponse); checked against real responses in the tests.
pub fn parse_eth_call(pc: &PerChain) -> Result<EthCall, &'static str> {
    if pc.kind != QT_ETH_CALL || pc.chain == SOLANA {
        return Err("not an EVM eth_call query");
    }
    let (mut q, mut r) = (Reader::new(&pc.request), Reader::new(&pc.response));
    let block_id = q.var()?.to_vec();
    let n = q.u8()?;
    let (block_num, block_hash, block_time_us) = (r.u64()?, r.b32()?, r.u64()?);
    if r.u8()? != n {
        return Err("result count");
    }
    let mut calls = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let to: [u8; 20] = q.bytes(20)?.try_into().unwrap();
        let data = q.var()?.to_vec();
        calls.push(Call { to, data, result: r.var()?.to_vec() });
    }
    q.done()?;
    r.done()?;
    Ok(EthCall { chain: pc.chain, block_id, block_num, block_hash, block_time_us, calls })
}

/// What the guardians sign for a query response.
pub fn response_digest(response: &[u8]) -> [u8; 32] {
    let mut m = RESPONSE_PREFIX.to_vec();
    m.extend_from_slice(&env::keccak256_array(response));
    env::keccak256_array(&m)
}

/// The Ethereum-style address of whoever signed `digest`: keccak256(pubkey)[12..].
pub fn signer(digest: &[u8; 32], rs: &[u8], v: u8) -> Option<[u8; 20]> {
    let v = if v >= 27 { v - 27 } else { v };
    let pk = env::ecrecover(digest, rs, v, false)?;
    Some(env::keccak256_array(&pk)[12..].try_into().unwrap())
}

/// More than two thirds of the set: 13 of 19.
pub fn quorum(n: usize) -> usize {
    n * 2 / 3 + 1
}

/// Signatures as (guardian index, r||s||v). Guardian indices must strictly
/// increase (so nobody counts twice), every one must recover to that guardian,
/// and there must be a quorum of them: Wormhole's, more than two thirds.
pub fn verify_quorum(digest: &[u8; 32], sigs: &[(u8, [u8; 65])], guardians: &[[u8; 20]]) -> Result<(), &'static str> {
    verify_sigs(digest, sigs, guardians, quorum)
}

/// As verify_quorum, with the set's own quorum rule: `need(set size)` signatures.
pub fn verify_sigs(digest: &[u8; 32], sigs: &[(u8, [u8; 65])], guardians: &[[u8; 20]], need: fn(usize) -> usize) -> Result<(), &'static str> {
    if guardians.is_empty() {
        return Err("no guardian set");
    }
    if sigs.len() < need(guardians.len()) {
        return Err("no quorum");
    }
    let mut last: Option<u8> = None;
    for (i, sig) in sigs {
        if last.is_some_and(|l| *i <= l) {
            return Err("guardian indices must increase");
        }
        last = Some(*i);
        let g = guardians.get(*i as usize).ok_or("no such guardian")?;
        if signer(digest, &sig[..64], sig[64]).as_ref() != Some(g) {
            return Err("bad guardian signature");
        }
    }
    Ok(())
}

/// A query proxy signature: 65 bytes r||s||v, then the guardian index (66 bytes, hex).
pub fn proxy_sig(hex_sig: &str) -> Result<(u8, [u8; 65]), &'static str> {
    let b = hex::decode(hex_sig).map_err(|_| "signature: hex")?;
    if b.len() != 66 {
        return Err("signature: 66 bytes");
    }
    Ok((b[65], b[..65].try_into().unwrap()))
}

/// A guardian set upgrade: index `index` with these guardians, approved by `signed_by`.
#[derive(Debug, Clone, PartialEq)]
pub struct Upgrade {
    pub signed_by: u32,
    pub index: u32,
    pub guardians: Vec<[u8; 20]>,
}

/// A VAA, split: who signed it, and the fields of the body they signed.
#[derive(Debug, Clone, PartialEq)]
pub struct Vaa<'a> {
    pub guardian_set: u32,
    /// (guardian index, r||s||v), as in verify_quorum.
    pub sigs: Vec<(u8, [u8; 65])>,
    /// What was signed: keccak256(keccak256(body)).
    pub digest: [u8; 32],
    pub timestamp: u32,
    pub emitter_chain: u16,
    pub emitter: [u8; 32],
    pub sequence: u64,
    pub payload: &'a [u8],
}

/// Version 1, the guardian set index, the signatures (each: guardian index,
/// then r||s||v), then the body: timestamp, nonce, emitter chain and address,
/// sequence, consistency level, payload. Signatures are not checked here.
pub fn parse_vaa(vaa: &[u8]) -> Result<Vaa<'_>, &'static str> {
    let mut r = Reader::new(vaa);
    if r.u8()? != 1 {
        return Err("VAA version");
    }
    let guardian_set = r.u32()?;
    let n = r.u8()?;
    let mut sigs = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let i = r.u8()?;
        sigs.push((i, r.bytes(65)?.try_into().unwrap()));
    }
    let body = &vaa[r.at..];
    let digest = env::keccak256_array(&env::keccak256_array(body));
    let mut b = Reader::new(body);
    let timestamp = b.u32()?;
    b.u32()?; // nonce
    let (emitter_chain, emitter, sequence) = (b.u16()?, b.b32()?, b.u64()?);
    b.u8()?; // consistency level
    Ok(Vaa { guardian_set, sigs, digest, timestamp, emitter_chain, emitter, sequence, payload: &body[b.at..] })
}

/// Parse and verify a governance VAA that names the next guardian set
/// ("Core" module, action 2, every chain), signed by a quorum of `current`.
pub fn guardian_upgrade(vaa: &[u8], current_index: u32, current: &[[u8; 20]]) -> Result<Upgrade, &'static str> {
    guardian_upgrade_with(vaa, current_index, current, quorum)
}

/// As guardian_upgrade, with the set's own quorum rule. Pyth's signer set
/// rotates with this same message, from the same governance address.
pub fn guardian_upgrade_with(vaa: &[u8], current_index: u32, current: &[[u8; 20]], need: fn(usize) -> usize) -> Result<Upgrade, &'static str> {
    let v = parse_vaa(vaa)?;
    let signed_by = v.guardian_set;
    if signed_by != current_index {
        return Err("VAA not signed by the current guardian set");
    }
    verify_sigs(&v.digest, &v.sigs, current, need)?;
    let mut governance = [0u8; 32];
    governance[31] = 4;
    if v.emitter_chain != SOLANA || v.emitter != governance {
        return Err("not from Wormhole governance");
    }
    let mut b = Reader::new(v.payload);
    let mut core = [0u8; 32];
    core[28..].copy_from_slice(b"Core");
    if b.b32()? != core || b.u8()? != 2 || b.u16()? != 0 {
        return Err("not a guardian set upgrade");
    }
    let index = b.u32()?;
    if index != current_index + 1 {
        return Err("guardian set index must be the next one");
    }
    let k = b.u8()?;
    let mut guardians = Vec::with_capacity(k as usize);
    for _ in 0..k {
        guardians.push(b.bytes(20)?.try_into().unwrap());
    }
    b.done()?;
    if guardians.is_empty() {
        return Err("empty guardian set");
    }
    Ok(Upgrade { signed_by, index, guardians })
}
