//! EVMTX: the vault BUILDS the Ethereum transaction it signs.
//!
//! On EVM chains the vault doesn't need anyone's bytes: from an owner-approved
//! template (which router, which tokens, recipient = the vault's own address,
//! a floor on the output) and a few run-time numbers from the keeper (nonce,
//! gas price, a quote), it assembles the transaction here and asks Chain
//! Signatures to sign its hash. A keeper can't redirect money: the "to", the
//! recipient and the minimum output are never its to choose.

use tiny_keccak::{Hasher, Keccak};

pub type Address = [u8; 20];

pub fn keccak256(b: &[u8]) -> [u8; 32] {
    let mut k = Keccak::v256();
    k.update(b);
    let mut out = [0u8; 32];
    k.finalize(&mut out);
    out
}

/// The address of an uncompressed secp256k1 public key (64 bytes x||y, or 65 with the 0x04 prefix).
pub fn address_of(pubkey: &[u8]) -> Option<Address> {
    let xy = match pubkey.len() {
        65 if pubkey[0] == 4 => &pubkey[1..],
        64 => pubkey,
        _ => return None,
    };
    Some(keccak256(xy)[12..].try_into().unwrap())
}

pub fn hex_address(a: &Address) -> String {
    let h: String = a.iter().map(|b| format!("{b:02x}")).collect();
    format!("0x{h}")
}

pub fn parse_address(s: &str) -> Option<Address> {
    let s = s.strip_prefix("0x")?;
    if s.len() != 40 {
        return None;
    }
    let mut a = [0u8; 20];
    for i in 0..20 {
        a[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(a)
}

/// RLP: the encoding Ethereum transactions use.
pub mod rlp {
    fn len_prefix(len: usize, short: u8, long: u8, out: &mut Vec<u8>) {
        if len <= 55 {
            out.push(short + len as u8);
        } else {
            let be = (len as u64).to_be_bytes();
            let skip = be.iter().take_while(|b| **b == 0).count();
            out.push(long + (8 - skip) as u8);
            out.extend(&be[skip..]);
        }
    }
    pub fn bytes(b: &[u8], out: &mut Vec<u8>) {
        if b.len() == 1 && b[0] < 0x80 {
            out.push(b[0]);
        } else {
            len_prefix(b.len(), 0x80, 0xb7, out);
            out.extend(b);
        }
    }
    /// An unsigned integer: big-endian, no leading zeros (0 = empty string).
    pub fn uint(v: u128, out: &mut Vec<u8>) {
        let be = v.to_be_bytes();
        let skip = be.iter().take_while(|b| **b == 0).count();
        bytes(&be[skip..], out);
    }
    pub fn list(items: &[Vec<u8>], out: &mut Vec<u8>) {
        let len: usize = items.iter().map(Vec::len).sum();
        len_prefix(len, 0xc0, 0xf7, out);
        for i in items {
            out.extend(i);
        }
    }
    pub fn item(f: impl FnOnce(&mut Vec<u8>)) -> Vec<u8> {
        let mut v = Vec::new();
        f(&mut v);
        v
    }
}

/// An EIP-1559 (type 2) transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Eip1559 {
    pub chain_id: u64,
    pub nonce: u64,
    pub max_priority_fee: u128,
    pub max_fee: u128,
    pub gas_limit: u64,
    pub to: Address,
    pub value: u128,
    pub data: Vec<u8>,
}

impl Eip1559 {
    fn fields(&self) -> Vec<Vec<u8>> {
        use rlp::*;
        vec![
            item(|o| uint(self.chain_id as u128, o)),
            item(|o| uint(self.nonce as u128, o)),
            item(|o| uint(self.max_priority_fee, o)),
            item(|o| uint(self.max_fee, o)),
            item(|o| uint(self.gas_limit as u128, o)),
            item(|o| bytes(&self.to, o)),
            item(|o| uint(self.value, o)),
            item(|o| bytes(&self.data, o)),
            item(|o| list(&[], o)), // empty access list
        ]
    }
    /// 0x02 || rlp([chain_id, nonce, tip, max_fee, gas, to, value, data, access_list])
    pub fn unsigned(&self) -> Vec<u8> {
        let mut out = vec![2u8];
        rlp::list(&self.fields(), &mut out);
        out
    }
    /// What gets signed.
    pub fn signing_hash(&self) -> [u8; 32] {
        keccak256(&self.unsigned())
    }
    /// The broadcastable transaction, with the signature (y parity 0/1, r, s).
    pub fn signed(&self, y_parity: u8, r: &[u8; 32], s: &[u8; 32]) -> Vec<u8> {
        use rlp::*;
        let strip = |b: &[u8; 32]| b.iter().skip_while(|x| **x == 0).copied().collect::<Vec<u8>>();
        let mut f = self.fields();
        f.push(item(|o| uint(y_parity as u128, o)));
        f.push(item(|o| bytes(&strip(r), o)));
        f.push(item(|o| bytes(&strip(s), o)));
        let mut out = vec![2u8];
        list(&f, &mut out);
        out
    }
}

/// Function signatures a template may name, parsed into ABI types: address,
/// uintN, intN, bool, bytes, tuples and dynamic arrays (T[]). No fixed-size
/// arrays or strings: nothing a DeFi call needs that these can't say.
pub mod sig {
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Ty {
        Address,
        Uint(u16),
        Int(u16),
        Bool,
        Bytes,
        Tuple(Vec<Ty>),
        Array(Box<Ty>),
    }

    impl Ty {
        pub fn is_dynamic(&self) -> bool {
            match self {
                Ty::Bytes | Ty::Array(_) => true,
                Ty::Tuple(ts) => ts.iter().any(Ty::is_dynamic),
                _ => false,
            }
        }
    }

    /// The argument types of e.g. "execute(bytes,bytes[],uint256)". None if malformed.
    pub fn args(sig: &str) -> Option<Vec<Ty>> {
        let open = sig.find('(')?;
        let name = &sig[..open];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return None;
        }
        list(&sig[open..])
    }

    /// "(t1,t2,...)" -> [t1, t2, ...]
    pub fn list(s: &str) -> Option<Vec<Ty>> {
        let (tys, rest) = tuple(s)?;
        rest.is_empty().then_some(tys)
    }

    fn tuple(s: &str) -> Option<(Vec<Ty>, &str)> {
        let mut s = s.strip_prefix('(')?;
        let mut out = Vec::new();
        if let Some(rest) = s.strip_prefix(')') {
            return Some((out, rest));
        }
        loop {
            let (t, rest) = one(s)?;
            out.push(t);
            if let Some(r) = rest.strip_prefix(',') {
                s = r;
            } else {
                return Some((out, rest.strip_prefix(')')?));
            }
        }
    }

    fn one(s: &str) -> Option<(Ty, &str)> {
        let (mut t, mut rest) = if s.starts_with('(') {
            let (tys, rest) = tuple(s)?;
            (Ty::Tuple(tys), rest)
        } else {
            let end = s.find([',', ')', '[']).unwrap_or(s.len());
            (base(&s[..end])?, &s[end..])
        };
        while let Some(r) = rest.strip_prefix("[]") {
            t = Ty::Array(Box::new(t));
            rest = r;
        }
        Some((t, rest))
    }

    fn base(t: &str) -> Option<Ty> {
        let bits = |p: &str| -> Option<u16> {
            let b: u16 = t.strip_prefix(p)?.parse().ok()?;
            (b % 8 == 0 && (8..=256).contains(&b)).then_some(b)
        };
        match t {
            "address" => Some(Ty::Address),
            "bool" => Some(Ty::Bool),
            "bytes" => Some(Ty::Bytes),
            _ if t.starts_with("uint") => bits("uint").map(Ty::Uint),
            _ if t.starts_with("int") => bits("int").map(Ty::Int),
            _ => None,
        }
    }
}

/// Standard ABI encoding of values already shaped like their types.
pub mod enc {
    use super::sig::Ty;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum Val {
        Word([u8; 32]),
        Bytes(Vec<u8>),
        Tuple(Vec<Val>),
        Array(Vec<Val>),
    }

    fn word_len(n: usize) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[24..].copy_from_slice(&(n as u64).to_be_bytes());
        w
    }

    /// abi.encode(v1, v2, ...) for types t1, t2, ... (a function's arguments, without the selector).
    /// Panics if a value doesn't have its type's shape (the vault builds them, so that's a bug).
    pub fn encode(tys: &[Ty], vals: &[Val]) -> Vec<u8> {
        assert_eq!(tys.len(), vals.len(), "one value per type");
        let head_len: usize = tys.iter().map(|t| if t.is_dynamic() { 32 } else { static_len(t) }).sum();
        let (mut head, mut tail) = (Vec::with_capacity(head_len), Vec::new());
        for (t, v) in tys.iter().zip(vals) {
            if t.is_dynamic() {
                head.extend(word_len(head_len + tail.len()));
                tail.extend(one(t, v));
            } else {
                head.extend(one(t, v));
            }
        }
        head.extend(tail);
        head
    }

    fn static_len(t: &Ty) -> usize {
        match t {
            Ty::Tuple(ts) => ts.iter().map(static_len).sum(),
            _ => 32,
        }
    }

    fn one(t: &Ty, v: &Val) -> Vec<u8> {
        match (t, v) {
            (Ty::Address | Ty::Uint(_) | Ty::Int(_) | Ty::Bool, Val::Word(w)) => w.to_vec(),
            (Ty::Bytes, Val::Bytes(b)) => {
                let mut out = word_len(b.len()).to_vec();
                out.extend(b);
                out.resize(32 + b.len().div_ceil(32) * 32, 0);
                out
            }
            (Ty::Tuple(ts), Val::Tuple(vs)) => encode(ts, vs),
            (Ty::Array(t), Val::Array(vs)) => {
                let mut out = word_len(vs.len()).to_vec();
                out.extend(encode(&vec![(**t).clone(); vs.len()], vs));
                out
            }
            _ => panic!("value doesn't match its type"),
        }
    }
}

/// ABI encoding of the two calls the vault makes.
pub mod abi {
    use super::{keccak256, Address};

    pub fn selector(sig: &str) -> [u8; 4] {
        keccak256(sig.as_bytes())[..4].try_into().unwrap()
    }
    fn word_addr(a: &Address) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[12..].copy_from_slice(a);
        w
    }
    fn word_uint(v: u128) -> [u8; 32] {
        let mut w = [0u8; 32];
        w[16..].copy_from_slice(&v.to_be_bytes());
        w
    }
    /// ERC-20 approve(spender, amount).
    pub fn approve(spender: &Address, amount: u128) -> Vec<u8> {
        let mut d = selector("approve(address,uint256)").to_vec();
        d.extend(word_addr(spender));
        d.extend(word_uint(amount));
        d
    }
    /// Uniswap SwapRouter02 exactInputSingle((tokenIn, tokenOut, fee, recipient, amountIn, amountOutMinimum, sqrtPriceLimitX96 = 0)).
    pub fn exact_input_single(token_in: &Address, token_out: &Address, fee: u32, recipient: &Address, amount_in: u128, amount_out_min: u128) -> Vec<u8> {
        let mut d = selector("exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))").to_vec();
        d.extend(word_addr(token_in));
        d.extend(word_addr(token_out));
        d.extend(word_uint(fee as u128));
        d.extend(word_addr(recipient));
        d.extend(word_uint(amount_in));
        d.extend(word_uint(amount_out_min));
        d.extend(word_uint(0));
        d
    }
}
