//! The vault BODY: one per user, published once as a global contract.
//!
//! WHY it exists: the full vault is ~600 KB, and publishing code costs 0.1 NEAR
//! per KB (59 NEAR per version). This body is a few KB. It holds the user's
//! money and positions and is the predecessor for Chain Signatures (so every
//! user keeps their own addresses on other chains), but it decides NOTHING.
//! All the rules live in one shared BRAIN contract, fixed when the vault is made.
//!
//! What it does, all of it:
//! - `x`: anyone may call it. It forwards the call (who called, what they
//!   attached, the raw args) to `brain.handle`, then executes the brain's answer.
//! - `cb`: executes a brain answer. Only ever runs as the vault's own callback
//!   on its own call to the brain (checked below).
//! - `new`: once, by whoever creates the account (the factory): fixes the brain.
//! - `ft_on_transfer`: accepts any token sent to it (it becomes free balance,
//!   exactly like the full vault). No brain round trip: nothing to decide.
//! - `brain`: view, which brain this vault obeys.
//!
//! Who can make it do what (the security argument):
//! - Promises are created in exactly two places: `x`/`new` (one fixed call to
//!   `brain.handle`, then `cb`) and `cb` (the actions of the brain's answer).
//! - `cb` refuses unless predecessor == this vault AND it has exactly one
//!   promise result AND that result succeeded. The only receipts this code
//!   ever points at `cb` are `then`s on its own calls to the brain, so a result
//!   reaching `cb` is the brain's answer, never anything a caller wrote.
//! - An answer can never target the vault itself, so it can't fake a `cb`
//!   (a direct self-call would also have no promise result and be refused).
//! - A stranger calling `x` makes the vault call `brain.handle` with the
//!   stranger's real name as sender; the brain refuses, `cb` sees the failure
//!   and does nothing. A stranger spends only their own gas (and any NEAR they
//!   attach becomes a gift to the vault).
//! - There is no code path that deploys code, adds a key, deletes the account
//!   or changes the brain: the wire format can only express function calls and
//!   transfers. Born keyless (factory), it can never be redeployed.
//! - The brain is trusted completely for this vault's money: it is one
//!   immutable contract (no keys), and its rules are the vault's rules.
//!
//! ## Wire format (all integers little-endian)
//!
//! Every call the vault makes to the brain carries this HEADER, then a body:
//! ```text
//! u8   version = 1
//! u128 vault balance (yocto, at the time of the call)
//! u64  vault storage usage (bytes)
//! u128 attached deposit (x/new: what the caller attached; callbacks: 0)
//! u64  gas left in the vault's receipt (x/new; callbacks: 0)
//! u8   sender length, then the sender (x/new: the real predecessor; callbacks: empty)
//! ...  body (x: the caller's raw args; new: the init args; callbacks: the brain's own args)
//! ```
//! The brain ANSWERS with:
//! ```text
//! u8   version = 1
//! u8   number of actions, then each action:
//!        u8 kind = 0 CALL:     u8 len + receiver, u8 len + method, u32 len + args, u128 deposit, u64 gas
//!        u8 kind = 1 TRANSFER: u8 len + receiver, u128 amount
//! u8   has callback (0/1); if 1:
//!        u8 len + brain method, u32 len + args, u64 gas
//!        -> the actions run in parallel, then brain.<method>(header + args)
//!           sees their results, then this vault's `cb` executes ITS answer.
//! u32  len + return value (returned to the caller when there's no callback)
//! ```
//! Without a callback the actions are fire-and-forget (like `detach`).

#![cfg_attr(target_arch = "wasm32", no_std)]

#[cfg(target_arch = "wasm32")]
mod host {
    #[link(wasm_import_module = "env")]
    extern "C" {
        pub fn read_register(register_id: u64, ptr: u64);
        pub fn register_len(register_id: u64) -> u64;
        pub fn current_account_id(register_id: u64);
        pub fn predecessor_account_id(register_id: u64);
        pub fn input(register_id: u64);
        pub fn storage_usage() -> u64;
        pub fn account_balance(balance_ptr: u64);
        pub fn attached_deposit(balance_ptr: u64);
        pub fn prepaid_gas() -> u64;
        pub fn used_gas() -> u64;
        pub fn value_return(value_len: u64, value_ptr: u64);
        pub fn panic_utf8(len: u64, ptr: u64) -> !;
        pub fn promise_and(promise_idx_ptr: u64, promise_idx_count: u64) -> u64;
        pub fn promise_batch_create(account_id_len: u64, account_id_ptr: u64) -> u64;
        pub fn promise_batch_then(promise_index: u64, account_id_len: u64, account_id_ptr: u64) -> u64;
        pub fn promise_batch_action_function_call_weight(
            promise_index: u64,
            function_name_len: u64,
            function_name_ptr: u64,
            arguments_len: u64,
            arguments_ptr: u64,
            amount_ptr: u64,
            gas: u64,
            weight: u64,
        );
        pub fn promise_batch_action_transfer(promise_index: u64, amount_ptr: u64);
        pub fn promise_results_count() -> u64;
        pub fn promise_result(result_idx: u64, register_id: u64) -> u64;
        pub fn promise_return(promise_id: u64);
        pub fn storage_write(key_len: u64, key_ptr: u64, value_len: u64, value_ptr: u64, register_id: u64) -> u64;
        pub fn storage_read(key_len: u64, key_ptr: u64, register_id: u64) -> u64;
    }
}

pub const VERSION: u8 = 1;
pub const KIND_CALL: u8 = 0;
pub const KIND_TRANSFER: u8 = 1;
pub const HEADER_FIXED: usize = 1 + 16 + 8 + 16 + 8;
pub const TGAS: u64 = 1_000_000_000_000;
/// What `brain.handle` gets for deciding a request. Unused gas is refunded.
pub const GAS_HANDLE: u64 = 30 * TGAS;
/// The least `cb` gets; it also gets ALL the unused gas (weight 1), because it
/// launches the next round of the brain's actions and they carry their own gas.
pub const GAS_CB: u64 = 10 * TGAS;
pub const MAX_ACTIONS: usize = 16;

/// A parsed action, borrowing from the answer bytes.
#[derive(Debug, PartialEq)]
pub enum Act<'a> {
    Call { to: &'a [u8], method: &'a [u8], args: &'a [u8], deposit: u128, gas: u64 },
    Transfer { to: &'a [u8], amount: u128 },
}

#[derive(Debug, PartialEq)]
pub struct Callback<'a> {
    pub method: &'a [u8],
    pub args: &'a [u8],
    pub gas: u64,
}

/// Reads the answer format. Pure, so it is unit tested off chain.
pub struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.at.checked_add(n)?;
        let s = self.b.get(self.at..end)?;
        self.at = end;
        Some(s)
    }
    pub fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    pub fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    pub fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }
    pub fn short(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    pub fn long(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    pub fn act(&mut self) -> Option<Act<'a>> {
        match self.u8()? {
            KIND_CALL => Some(Act::Call { to: self.short()?, method: self.short()?, args: self.long()?, deposit: self.u128()?, gas: self.u64()? }),
            KIND_TRANSFER => Some(Act::Transfer { to: self.short()?, amount: self.u128()? }),
            _ => None,
        }
    }
    pub fn callback(&mut self) -> Option<Option<Callback<'a>>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(Callback { method: self.short()?, args: self.long()?, gas: self.u64()? })),
            _ => None,
        }
    }
    pub fn done(&self) -> bool {
        self.at == self.b.len()
    }
}

#[cfg(target_arch = "wasm32")]
mod contract {
    use super::*;
    use core::ptr::addr_of_mut;
    use host::*;

    // Static buffers instead of an allocator: zero-initialised memory costs no
    // code bytes. A request (header + args) and an answer must each fit.
    static mut OUT: [u8; 128 * 1024] = [0; 128 * 1024];
    static mut ANS: [u8; 64 * 1024] = [0; 64 * 1024];
    static mut ME: [u8; 64] = [0; 64];
    static mut BRAIN: [u8; 64] = [0; 64];

    const KEY_BRAIN: &[u8] = b"b";

    fn fail(msg: &str) -> ! {
        unsafe { panic_utf8(msg.len() as u64, msg.as_ptr() as u64) }
    }

    #[panic_handler]
    fn on_panic(_: &core::panic::PanicInfo) -> ! {
        fail("relay panicked")
    }

    /// Read register 0 into `buf`; returns the length.
    fn reg_into(buf: &mut [u8]) -> usize {
        unsafe {
            let n = register_len(0) as usize;
            if n > buf.len() {
                fail("too big");
            }
            read_register(0, buf.as_mut_ptr() as u64);
            n
        }
    }

    fn me() -> &'static [u8] {
        unsafe {
            current_account_id(0);
            let b = &mut *addr_of_mut!(ME);
            let n = reg_into(b);
            &b[..n]
        }
    }

    fn brain() -> &'static [u8] {
        unsafe {
            if storage_read(KEY_BRAIN.len() as u64, KEY_BRAIN.as_ptr() as u64, 0) == 0 {
                fail("not initialised");
            }
            let b = &mut *addr_of_mut!(BRAIN);
            let n = reg_into(b);
            &b[..n]
        }
    }

    /// Write the header into OUT. `caller`: (sender in register 0 already read
    /// by the caller, deposit, gas left) for requests; None for callbacks.
    /// Returns the header length; the body goes right after it.
    fn header(request: bool) -> usize {
        unsafe {
            let o = &mut *addr_of_mut!(OUT);
            o[0] = VERSION;
            account_balance(o.as_mut_ptr().add(1) as u64);
            o[17..25].copy_from_slice(&storage_usage().to_le_bytes());
            if request {
                attached_deposit(o.as_mut_ptr().add(25) as u64);
                o[41..49].copy_from_slice(&prepaid_gas().saturating_sub(used_gas()).to_le_bytes());
                predecessor_account_id(0);
                let n = register_len(0) as usize;
                o[49] = n as u8;
                read_register(0, o.as_mut_ptr().add(50) as u64);
                50 + n
            } else {
                o[25..49].fill(0);
                o[49] = 0;
                50
            }
        }
    }

    /// brain.<method>(OUT[..len]) with `gas`, then this vault's `cb`, after `after` (if any).
    fn ask_brain(after: Option<u64>, method: &[u8], len: usize, gas: u64) -> u64 {
        let b = brain();
        let zero = 0u128.to_le_bytes();
        unsafe {
            let p = match after {
                Some(dep) => promise_batch_then(dep, b.len() as u64, b.as_ptr() as u64),
                None => promise_batch_create(b.len() as u64, b.as_ptr() as u64),
            };
            let out = &*addr_of_mut!(OUT);
            promise_batch_action_function_call_weight(p, method.len() as u64, method.as_ptr() as u64, len as u64, out.as_ptr() as u64, zero.as_ptr() as u64, gas, 0);
            let m = me();
            let c = promise_batch_then(p, m.len() as u64, m.as_ptr() as u64);
            promise_batch_action_function_call_weight(c, 2, b"cb".as_ptr() as u64, 0, 0, zero.as_ptr() as u64, GAS_CB, 1);
            c
        }
    }

    /// Forward the caller's request (the raw input) to brain.handle.
    fn forward() {
        let h = header(true);
        unsafe {
            input(0);
            let out = &mut *addr_of_mut!(OUT);
            let n = reg_into(&mut out[h..]);
            let c = ask_brain(None, b"handle", h + n, GAS_HANDLE);
            promise_return(c);
        }
    }

    /// Once, by whoever creates the account. Input: u8 len + brain account, then
    /// the init request forwarded to the brain (which checks it came from the factory).
    #[no_mangle]
    pub extern "C" fn new() {
        unsafe {
            if storage_read(KEY_BRAIN.len() as u64, KEY_BRAIN.as_ptr() as u64, 0) != 0 {
                fail("already initialised");
            }
            input(0);
            let ans = &mut *addr_of_mut!(ANS);
            let n = reg_into(ans);
            let mut r = Reader::new(&ans[..n]);
            let b = r.short().unwrap_or_else(|| fail("bad init"));
            if b.is_empty() || b.len() > 64 {
                fail("bad brain");
            }
            storage_write(KEY_BRAIN.len() as u64, KEY_BRAIN.as_ptr() as u64, b.len() as u64, b.as_ptr() as u64, 0);
            let rest = &ans[1 + b.len()..n];
            let h = header(true);
            let out = &mut *addr_of_mut!(OUT);
            if h + rest.len() > out.len() {
                fail("too big");
            }
            out[h..h + rest.len()].copy_from_slice(rest);
            let c = ask_brain(None, b"handle", h + rest.len(), GAS_HANDLE);
            promise_return(c);
        }
    }

    /// Any request, from anyone: the brain decides.
    #[no_mangle]
    pub extern "C" fn x() {
        forward();
    }

    /// Execute the brain's answer. Only as our own callback on our own brain call.
    #[no_mangle]
    pub extern "C" fn cb() {
        unsafe {
            predecessor_account_id(0);
            let mut pre = [0u8; 64];
            let pn = reg_into(&mut pre);
            let m = me();
            if &pre[..pn] != m {
                fail("only the vault's own callback");
            }
            if promise_results_count() != 1 {
                fail("not a brain answer");
            }
            if promise_result(0, 0) != 1 {
                fail("the brain refused");
            }
            let ans = &mut *addr_of_mut!(ANS);
            let n = reg_into(ans);
            run(&ans[..n], m);
        }
    }

    fn run(a: &[u8], m: &[u8]) {
        let mut r = Reader::new(a);
        if r.u8() != Some(VERSION) {
            fail("bad answer");
        }
        let count = r.u8().unwrap_or_else(|| fail("bad answer")) as usize;
        if count > MAX_ACTIONS {
            fail("too many actions");
        }
        let mut ids = [0u64; MAX_ACTIONS];
        for i in 0..count {
            let act = r.act().unwrap_or_else(|| fail("bad action"));
            let to = match act {
                Act::Call { to, .. } | Act::Transfer { to, .. } => to,
            };
            // Never ourselves: no faked `cb`, no calls that skip the brain.
            if to == m || to.is_empty() {
                fail("an action can't target the vault itself");
            }
            unsafe {
                let p = promise_batch_create(to.len() as u64, to.as_ptr() as u64);
                match act {
                    Act::Call { method, args, deposit, gas, .. } => {
                        let d = deposit.to_le_bytes();
                        promise_batch_action_function_call_weight(p, method.len() as u64, method.as_ptr() as u64, args.len() as u64, args.as_ptr() as u64, d.as_ptr() as u64, gas, 0)
                    }
                    Act::Transfer { amount, .. } => {
                        let d = amount.to_le_bytes();
                        promise_batch_action_transfer(p, d.as_ptr() as u64)
                    }
                }
                ids[i] = p;
            }
        }
        let cbk = r.callback().unwrap_or_else(|| fail("bad answer"));
        let ret = r.long().unwrap_or_else(|| fail("bad answer"));
        if !r.done() {
            fail("bad answer");
        }
        match cbk {
            None => unsafe { value_return(ret.len() as u64, ret.as_ptr() as u64) },
            Some(c) => {
                let h = header(false);
                unsafe {
                    let out = &mut *addr_of_mut!(OUT);
                    if h + c.args.len() > out.len() {
                        fail("too big");
                    }
                    out[h..h + c.args.len()].copy_from_slice(c.args);
                    let after = match count {
                        0 => None,
                        1 => Some(ids[0]),
                        _ => Some(promise_and(ids.as_ptr() as u64, count as u64)),
                    };
                    let p = ask_brain(after, c.method, h + c.args.len(), c.gas);
                    promise_return(p);
                }
            }
        }
    }

    /// NEP-141: keep every token sent to us (it becomes free balance).
    #[no_mangle]
    pub extern "C" fn ft_on_transfer() {
        unsafe { value_return(3, b"\"0\"".as_ptr() as u64) }
    }

    /// View: the brain this vault obeys, as a JSON string.
    #[no_mangle]
    pub extern "C" fn brain_id() {
        let b = brain();
        let out = unsafe { &mut *addr_of_mut!(OUT) };
        out[0] = b'"';
        out[1..1 + b.len()].copy_from_slice(b);
        out[1 + b.len()] = b'"';
        unsafe { value_return(b.len() as u64 + 2, out.as_ptr() as u64) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(to: &str, method: &str, args: &[u8], deposit: u128, gas: u64) -> Vec<u8> {
        let mut v = vec![KIND_CALL, to.len() as u8];
        v.extend(to.as_bytes());
        v.push(method.len() as u8);
        v.extend(method.as_bytes());
        v.extend((args.len() as u32).to_le_bytes());
        v.extend(args);
        v.extend(deposit.to_le_bytes());
        v.extend(gas.to_le_bytes());
        v
    }

    #[test]
    fn an_answer_reads_back_exactly_as_written() {
        let mut a = vec![VERSION, 2];
        a.extend(call("usdc.near", "ft_balance_of", b"{}", 0, 5 * TGAS));
        a.extend([KIND_TRANSFER, 5]);
        a.extend(b"owner");
        a.extend(7u128.to_le_bytes());
        a.extend([1, 2]);
        a.extend(b"on");
        a.extend(3u32.to_le_bytes());
        a.extend(b"abc");
        a.extend(9u64.to_le_bytes());
        a.extend(0u32.to_le_bytes());
        let mut r = Reader::new(&a);
        assert_eq!(r.u8(), Some(VERSION));
        assert_eq!(r.u8(), Some(2));
        assert_eq!(r.act(), Some(Act::Call { to: b"usdc.near", method: b"ft_balance_of", args: b"{}", deposit: 0, gas: 5 * TGAS }));
        assert_eq!(r.act(), Some(Act::Transfer { to: b"owner", amount: 7 }));
        assert_eq!(r.callback(), Some(Some(Callback { method: b"on", args: b"abc", gas: 9 })));
        assert_eq!(r.long(), Some(&b""[..]));
        assert!(r.done());
    }

    #[test]
    fn a_truncated_or_unknown_action_is_refused() {
        let a = call("usdc.near", "m", b"{}", 0, 1);
        assert!(Reader::new(&a[..a.len() - 1]).act().is_none());
        assert!(Reader::new(&[7u8, 0]).act().is_none(), "kind 7 doesn't exist (no deploy, no keys)");
        assert!(Reader::new(&[2u8]).callback().is_none());
    }
}
