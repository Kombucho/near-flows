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
