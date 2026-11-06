//! The brain's side of the relay wire format (documented in relay/src/lib.rs):
//! reading the header every vault call carries, and writing answers.

use near_sdk::borsh::{BorshDeserialize, BorshSerialize};
use near_sdk::{env, AccountId, Gas};

pub const VERSION: u8 = 1;

/// What the vault reports about itself and its caller. `gas_left` is rewritten by the
/// brain on entry to "what the vault's next round will have" (see `Talk::open`).
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub struct Ctx {
    pub balance: u128,
    pub storage: u64,
    pub deposit: u128,
    pub gas_left: u64,
    /// Who called the vault (requests). None on callbacks.
    pub sender: Option<AccountId>,
}

/// Split a vault call's input into its header and body.
pub fn parse(input: &[u8]) -> (Ctx, &[u8]) {
    let bad = || env::panic_str("bad header");
    if input.len() < 50 || input[0] != VERSION {
        bad();
    }
    let u128_at = |i: usize| u128::from_le_bytes(input[i..i + 16].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(input[i..i + 8].try_into().unwrap());
    let n = input[49] as usize;
    if input.len() < 50 + n {
        bad();
    }
    let sender = if n == 0 {
        None
    } else {
        Some(std::str::from_utf8(&input[50..50 + n]).ok().and_then(|s| s.parse().ok()).unwrap_or_else(|| bad()))
    };
    (Ctx { balance: u128_at(1), storage: u64_at(17), deposit: u128_at(25), gas_left: u64_at(41), sender }, &input[50 + n..])
}

/// An answer for the vault to execute.
#[derive(Default)]
pub struct Answer {
    n: u8,
    acts: Vec<u8>,
    cb: Option<(String, Vec<u8>, Gas)>,
    ret: Vec<u8>,
}

impl Answer {
    pub fn call(mut self, to: &AccountId, method: &str, args: &[u8], deposit: u128, gas: Gas) -> Self {
        self.n += 1;
        self.acts.push(0);
        short(&mut self.acts, to.as_bytes());
        short(&mut self.acts, method.as_bytes());
        self.acts.extend((args.len() as u32).to_le_bytes());
        self.acts.extend(args);
        self.acts.extend(deposit.to_le_bytes());
        self.acts.extend(gas.as_gas().to_le_bytes());
        self
    }

    pub fn transfer(mut self, to: &AccountId, amount: u128) -> Self {
        self.n += 1;
        self.acts.push(1);
        short(&mut self.acts, to.as_bytes());
        self.acts.extend(amount.to_le_bytes());
        self
    }

    /// After the actions: brain.<method>(args) sees their results.
    pub fn then(mut self, method: &str, args: Vec<u8>, gas: Gas) -> Self {
        self.cb = Some((method.to_string(), args, gas));
        self
    }

    /// What the vault returns to its caller (JSON), when the answer has no callback.
    pub fn ret(mut self, json: &str) -> Self {
        self.ret = json.as_bytes().to_vec();
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut v = vec![VERSION, self.n];
        v.extend(&self.acts);
        match &self.cb {
            None => v.push(0),
            Some((m, a, g)) => {
                v.push(1);
                short(&mut v, m.as_bytes());
                v.extend((a.len() as u32).to_le_bytes());
                v.extend(a);
                v.extend(g.as_gas().to_le_bytes());
            }
        }
        v.extend((self.ret.len() as u32).to_le_bytes());
        v.extend(&self.ret);
        v
    }

    pub fn send(self) {
        env::value_return(&self.encode());
    }
}

fn short(v: &mut Vec<u8>, b: &[u8]) {
    v.push(b.len() as u8);
    v.extend(b);
}

#[cfg(test)]
mod tests {
    use super::*;
    use relay::{Act, Callback, Reader};

    #[test]
    fn the_relay_reads_exactly_what_the_brain_writes() {
        let usdc: AccountId = "usdc.near".parse().unwrap();
        let a = Answer::default()
            .call(&usdc, "ft_balance_of", br#"{"account_id":"v"}"#, 0, Gas::from_tgas(5))
            .transfer(&"alice.near".parse().unwrap(), 42)
            .then("on_measured", vec![1, 2, 3], Gas::from_tgas(15))
            .ret("7")
            .encode();
        let mut r = Reader::new(&a);
        assert_eq!((r.u8(), r.u8()), (Some(VERSION), Some(2)));
        assert_eq!(r.act(), Some(Act::Call { to: b"usdc.near", method: b"ft_balance_of", args: br#"{"account_id":"v"}"#, deposit: 0, gas: 5_000_000_000_000 }));
        assert_eq!(r.act(), Some(Act::Transfer { to: b"alice.near", amount: 42 }));
        assert_eq!(r.callback(), Some(Some(Callback { method: b"on_measured", args: &[1, 2, 3], gas: 15_000_000_000_000 })));
        assert_eq!(r.long(), Some(&b"7"[..]));
        assert!(r.done());
    }
}
