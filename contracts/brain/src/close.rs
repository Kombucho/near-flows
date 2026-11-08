//! Closing a vault: when its job is done it deletes itself and its NEAR goes
//! back to the owner. Opening one costs ~0.1 NEAR (the account's own storage,
//! the reserve, its record here); closing returns all of it but the gas.
//!
//! Who may close:
//! - the owner, any time (a stealth owner through a signed `exec`, naming
//!   where the NEAR goes: a stealth vault has no owner account);
//! - its keeper, once the owner turned auto-close on (after the last step):
//!   the NEAR can only go where the owner said. Nobody else: a stranger never
//!   picks the moment a vault disappears.
//!
//! What must be true first, all checked here (refused otherwise):
//! - no step running, no remote step waiting for its report;
//! - no flow holds anything (every node's inbox and running amount zero,
//!   hidden flows included);
//! - every contract the vault EVER had to do with reads `ft_balance_of(vault)
//!   == 0`: trusted (with or without a method list, removed since or not),
//!   named by a flow (deleted since or not, a hidden node once revealed to
//!   run), funded, withdrawn. One used as a token must answer (a failed read
//!   refuses); one only trusted or called may be no token (a failed read is
//!   no balance, a balance still refuses);
//! - with Intents set up, its balances of the allowed pairs' tokens are zero.
//! A vault that EVER set up another chain (EVM, Solana, Intents, a remote node)
//! can't auto-close: its addresses there are derived from its name and nobody
//! here can see what they hold. Nor can a vault that may hold funds INSIDE
//! another contract (`elsewhere`): it trusted a contract with a method list
//! (a stake, a lending deposit, an unstake waiting: positions only that
//! contract knows of), or ran a node whose output wasn't measured (`out:
//! null`, or input spent for nothing back: a deposit kept by the receiver).
//! Only the owner closes those, with `confirm_remote: true` ("I swept the other
//! chains and the positions"); the balance reads above still apply.
//!
//! Then the factory tombstones the name (a new vault with this name would
//! inherit the derived addresses and anything stranded at them) and, only once
//! that succeeded, the record is deleted, what the vault paid for it is refunded
//! to the same beneficiary, and the answer is the relay's DELETE action alone.
//! A tombstone that fails refuses the close: nothing is deleted. The brain keeps
//! its own tombstone too (a few bytes, out of the refund) and never gives the
//! name a record again: a creation of the name already on its way when the
//! factory wrote its tombstone (past the factory's check, its receipt delayed
//! until after the delete) is refused here, so that vault deletes itself.
//!
//! What can't be checked: a token the vault never had to do with (anyone may
//! send one: the relay keeps tokens without asking the brain), and tokens
//! arriving after the balance reads. They stay registered to a deleted
//! account. The owner should sweep first.

use near_sdk::json_types::U128;
use near_sdk::{env, require, AccountId, Gas, NearToken, Promise};

use flow::Node;

use crate::wire::Answer;
use crate::{remote, Brain, Call, Read, ReadArgs, Recipe, Reply, Talk, NEAR};

/// on_read for a close: decides (2.5-4 Tgas like any on_read), then asks the
/// factory for the tombstone (GAS_TOMBSTONE) and waits for it (GAS_DELETE),
/// each ~2.5 Tgas of receipt fees on top.
const GAS_CLOSE: Gas = Gas::from_tgas(30);
/// The factory's `on_vault_closed`: a set insert and a map removal.
const GAS_TOMBSTONE: Gas = Gas::from_tgas(8);
/// The tombstone's callback: deletes the record, makes the refund (a transfer), answers DELETE.
const GAS_DELETE: Gas = Gas::from_tgas(10);
/// At most this many balance reads in one close.
const MAX_READS: usize = 20;
/// Contracts the vault remembers for its close (one read left for Intents). Past
/// this, only its owner closes it.
const MAX_TOKENS: usize = MAX_READS - 1;
/// Why a stranger can't close it.
const ELSEWHERE: &str = "this vault may hold funds inside other contracts (it may call their methods, or ran a node whose output wasn't measured)";

impl Talk {
    pub(crate) fn set_auto_close(&mut self, on: bool, to: Option<AccountId>) {
        if !on {
            self.vs.auto_close = None;
            return;
        }
        require!(!self.vs.crosschain, "this vault used other chains: close it yourself with close(confirm_remote: true)");
        require!(!self.vs.elsewhere, format!("{ELSEWHERE}: close it yourself with close(confirm_remote: true)"));
        self.vs.auto_close = Some(self.beneficiary(to));
    }

    /// Where the NEAR goes: a named vault's owner; a stealth vault's signed `to`.
    fn beneficiary(&self, to: Option<AccountId>) -> AccountId {
        if self.vs.stealth.is_some() {
            let to = to.unwrap_or_else(|| env::panic_str("a stealth vault closes to an account you name: to"));
            require!(to != self.vault, "to: another account");
            return to;
        }
        require!(to.is_none() || to.as_ref() == Some(&self.vs.owner), "a vault closes to its owner");
        self.vs.owner.clone()
    }

    pub(crate) fn close(&mut self, by_owner: bool, by_keeper: bool, confirm_remote: bool, to: Option<AccountId>) -> Reply {
        let to = if by_owner {
            self.beneficiary(to)
        } else {
            let b = self.vs.auto_close.clone().unwrap_or_else(|| env::panic_str("auto-close is off: only the owner closes this vault"));
            require!(to.is_none() || to.as_ref() == Some(&b), "the vault closes only where its owner said");
            require!(by_keeper, "only the owner or its keeper closes this vault");
            b
        };
        if self.vs.crosschain {
            require!(by_owner && confirm_remote, "this vault used other chains: only its owner closes it, with confirm_remote: true");
        }
        self.idle();
        require!(self.vs.remote.is_empty(), "a remote step is waiting for its report");
        for (id, f) in self.vs.flows.iter().enumerate() {
            if let Some(f) = f {
                require!(f.ledger.nodes.iter().all(|s| s.inbox == 0 && s.running == 0), format!("flow {id} still holds funds: drain it first"));
            }
        }
        if self.vs.elsewhere {
            require!(by_owner && confirm_remote, format!("{ELSEWHERE}: only its owner closes it, with confirm_remote: true"));
        }
        let tokens = self.vs.tokens.clone();
        let mut views: Vec<(AccountId, String, String)> = tokens.iter().map(|(t, _)| self.balance_view(t)).collect();
        let mut intents = Vec::new();
        if let Some(cfg) = &self.vs.intents {
            for p in &cfg.pairs {
                for t in [&p.token_in, &p.token_out] {
                    let id = format!("nep141:{t}");
                    if !intents.contains(&id) {
                        intents.push(id);
                    }
                }
            }
            if !intents.is_empty() {
                let args = near_sdk::serde_json::json!({"account_id": self.vault, "token_ids": intents}).to_string();
                views.push((cfg.verifier.clone(), "mt_batch_balance_of".into(), args));
            }
        }
        require!(views.len() <= MAX_READS, "too many tokens to check in one close");
        self.read(views, Read::Close { to, tokens, intents }, GAS_CLOSE)
    }

    /// Remember a contract the vault may hold a token of (`strong`: used as a
    /// token). NEAR, "none" and other chains' assets aren't contracts here.
    pub(crate) fn note_token(&mut self, t: &str, strong: bool) {
        if t == NEAR || t == flow::NO_INPUT || remote::is_remote(t) {
            return;
        }
        let Ok(a) = t.parse::<AccountId>() else { return };
        if let Some(e) = self.vs.tokens.iter_mut().find(|(x, _)| *x == a) {
            e.1 |= strong;
        } else if self.vs.tokens.len() < MAX_TOKENS {
            self.vs.tokens.push((a, strong));
        } else {
            self.vs.elsewhere = true; // too many to read in one close
        }
    }

    /// A node's tokens (in, out) and the contracts it sends to or calls.
    pub(crate) fn note_node(&mut self, n: &Node<Recipe>) {
        self.note_token(&n.input, true);
        if let Some(o) = n.action.out.clone() {
            self.note_token(o.as_str(), true);
        }
        for c in &n.action.calls {
            match c {
                Call::Ft { receiver, .. } => self.note_token(receiver.as_str(), false),
                Call::Fn { contract, .. } => self.note_token(contract.as_str(), false),
            }
        }
    }

    pub(crate) fn on_close(&mut self, to: AccountId, tokens: Vec<(AccountId, bool)>, intents: Vec<String>, factory: &AccountId) -> Reply {
        for (i, (t, strong)) in tokens.iter().enumerate() {
            match crate::amount_result(i as u64) {
                Some(0) => {}
                Some(n) => env::panic_str(&format!("refused: the vault holds {n} of {t}: withdraw it first")),
                None if *strong => env::panic_str(&format!("refused: couldn't read the vault's balance of {t}")),
                None => {} // trusted or called, no token
            }
        }
        if !intents.is_empty() {
            let raw = crate::result(tokens.len() as u64, 4096).unwrap_or_else(|| env::panic_str("refused: couldn't read the Intents balances"));
            let bals: Vec<U128> = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("refused: bad Intents balances"));
            require!(bals.iter().all(|b| b.0 == 0), "refused: the vault still holds tokens on Intents: withdraw them first");
        }
        // Still idle? (Another request may have landed between the reads and now.)
        self.idle();
        // The name's tombstone first, the delete only once it's written: a vault
        // gone without one could be made again under its name, and its derived
        // addresses on other chains with it.
        let owner = (self.vs.stealth.is_none()).then(|| self.vs.owner.clone());
        let args = near_sdk::serde_json::json!({"vault": self.vault, "owner": owner}).to_string();
        let read = ReadArgs { vault: self.vault.clone(), ctx: self.ctx.clone(), read: Read::Tombstoned { to } };
        let p = Promise::new(factory.clone())
            .function_call("on_vault_closed".to_string(), args.into_bytes(), NearToken::from_yoctonear(0), GAS_TOMBSTONE)
            .then(Brain::ext(env::current_account_id()).with_static_gas(GAS_DELETE).on_read(read));
        Reply::Read(p)
    }

    /// The factory tombstoned the name (or couldn't: then nothing is deleted, try again).
    pub(crate) fn on_tombstoned(&mut self, to: AccountId) -> Reply {
        require!(crate::result(0, 64).is_some(), "refused: the factory couldn't tombstone the vault's name: nothing was deleted, try again");
        // Still idle? (Another request may have landed since the reads.) If not, the
        // name stays tombstoned and the vault keeps working; its owner closes it later.
        self.idle();
        env::storage_remove(&crate::key(&self.vault));
        env::storage_write(&crate::tomb_key(&self.vault), b"");
        let refund = self.vs.paid.0.saturating_sub(crate::tomb_cost(&self.vault));
        if refund > 0 {
            Promise::new(to.clone()).transfer(NearToken::from_yoctonear(refund)).detach();
        }
        self.emit("vault_closed", &format!(r#""to":"{to}","record_refund":"{refund}""#));
        Reply::Final(Answer::default().delete(&to))
    }
}
