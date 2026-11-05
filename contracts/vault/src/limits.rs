//! Spending limits: "flows may spend at most X of this token per period".
//!
//! Every step's input counts against its token's limit, whoever runs the step
//! (owner, keeper, or an agent holding a limited key). Tightening a limit
//! takes effect at once; loosening or removing one only after LOOSEN_DELAY_NS,
//! so a hijacked agent can't raise the cap and drain the vault in one go.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require};

use crate::{Vault, VaultExt};

pub const LOOSEN_DELAY_NS: u64 = 24 * 3_600 * 1_000_000_000;
pub const MIN_WINDOW_NS: u64 = 60 * 1_000_000_000;

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Limit {
    /// "near" or a token contract.
    pub token: String,
    pub max: U128,
    pub window_ns: u64,
    pub window_start_ns: u64,
    pub used: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingLimit {
    pub token: String,
    /// None = remove the limit.
    pub max: Option<U128>,
    pub window_ns: u64,
    pub active_at_ns: u64,
}

#[near]
impl Vault {
    /// Set, tighten, loosen or remove (`max: null`) a token's spending limit.
    pub fn set_limit(&mut self, token: String, max: Option<U128>, window_ns: u64) {
        self.assert_owner();
        let now = env::block_timestamp();
        let current = self.limits.iter().find(|l| l.token == token).cloned();
        let tighter = match (&current, max) {
            (_, None) => false,
            (None, Some(_)) => true,
            (Some(c), Some(m)) => m.0 <= c.max.0 && window_ns >= c.window_ns,
        };
        if max.is_some() {
            require!(window_ns >= MIN_WINDOW_NS, "window: at least a minute");
        }
        self.pending.retain(|p| p.token != token);
        if tighter {
            self.put_limit(token, max.unwrap(), window_ns, current);
        } else {
            self.pending.push(PendingLimit { token, max, window_ns, active_at_ns: now + LOOSEN_DELAY_NS });
        }
    }

    /// Apply a loosened limit once its delay has passed. Anyone may call it.
    pub fn apply_limit(&mut self, token: String) {
        let now = env::block_timestamp();
        let i = self.pending.iter().position(|p| p.token == token).unwrap_or_else(|| env::panic_str("nothing pending"));
        require!(now >= self.pending[i].active_at_ns, "not yet: loosening a limit waits 24h");
        let p = self.pending.remove(i);
        let current = self.limits.iter().find(|l| l.token == token).cloned();
        match p.max {
            Some(m) => self.put_limit(token, m, p.window_ns, current),
            None => self.limits.retain(|l| l.token != token),
        }
    }

    pub fn get_limits(&self) -> (Vec<Limit>, Vec<PendingLimit>) {
        (self.limits.clone(), self.pending.clone())
    }
}

impl Vault {
    fn put_limit(&mut self, token: String, max: U128, window_ns: u64, current: Option<Limit>) {
        // Keep what was already used in the current window.
        let (start, used) = current.map_or((env::block_timestamp(), U128(0)), |c| (c.window_start_ns, c.used));
        self.limits.retain(|l| l.token != token);
        require!(self.limits.len() < 32, "too many limits");
        self.limits.push(Limit { token, max, window_ns, window_start_ns: start, used });
    }

    /// Count `amount` of `token` against its limit; panics if it would go over.
    pub(crate) fn charge(&mut self, token: &str, amount: u128) {
        let now = env::block_timestamp();
        let Some(l) = self.limits.iter_mut().find(|l| l.token == token) else { return };
        if now >= l.window_start_ns.saturating_add(l.window_ns) {
            l.window_start_ns = now;
            l.used = U128(0);
        }
        let used = l.used.0.saturating_add(amount);
        if used > l.max.0 {
            env::panic_str(&format!("spending limit: {} of {} left this period", l.max.0 - l.used.0, token));
        }
        l.used = U128(used);
    }
}
