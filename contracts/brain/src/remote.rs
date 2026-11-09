//! Remote nodes: a flow node whose action is a
//! transaction on another chain (an EVM template, a Solana template).
//!
//! The brain decides everything the vault decided: the trigger (block clock,
//! or oracle prices the brain reads itself), the amount (the node's own input,
//! put into the template's amount leaf / checked in the message), the minimum
//! (the template's floor or market rule, enforced by the signed transaction),
//! an EVM deadline (now + 5 min) and the vault's next EVM nonce. It can't see
//! balances on other chains, so a remote node's output is KEEPER-REPORTED
//! (`remote_report`), accepted only between the signed minimum and the node's
//! cap; only the owner may report a signed transaction failed (that frees its
//! input). Same bounds, same events.
//!
//! What changed in the split: the pending steps live in the vault's record
//! (`VaultState.remote`), and the signer's answer comes back through the vault
//! (`on_vault` RemoteEvmSigned / RemoteSolSigned), since only the vault may sign.
//!
//! A HOP node (hop.rs) is a remote node too: its input is a token in the vault's
//! Intents balance (`intents:<token>`), its action the bridge withdrawal to the
//! vault's own address on the other chain, its output what landed there
//! (keeper-reported, between the amount less the owner's fee bound and the amount).

use flow::{Node, Trigger};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use crate::evm::{Arg, EvmTemplate, TxJson};
use crate::wire::Answer;
use crate::{Next, Read, Recipe, Reply, Talk, PRICE_SCALE};

pub const EVM_TTL_S: u64 = 300;
pub const SOL_TTL_S: u64 = 180;
/// on_read after the oracle: checks the trigger, builds the node's transaction and
/// answers the signature request; burns 5.3 Tgas, 14 is a ~2.5x margin.
const GAS_BUILD: Gas = Gas::from_tgas(14);
/// The callback with the signature: burns 4.9 Tgas; 12 is a ~2.5x margin.
const GAS_SIGNED_CB: Gas = Gas::from_tgas(12);
/// The vault's rounds after the brain's answer: its `sign`, the brain's callback, its own.
const NEED_TGAS: u64 = 50 + 12 + crate::RELAY_TGAS;
/// A hop's: the verifier's withdrawal, the brain's callback, the vault's own.
const HOP_NEED_TGAS: u64 = crate::hop::GAS_HOP.as_tgas() + crate::hop::GAS_HOP_SENT.as_tgas() + crate::RELAY_TGAS;

#[near(serializers = [borsh, json])]
#[serde(tag = "chain", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteCall {
    Evm {
        template: String,
        #[serde(default)]
        amount_leaf: Option<u8>,
        #[serde(default)]
        min_leaf: Option<u8>,
        #[serde(default)]
        deadline_leaf: Option<u8>,
    },
    Sol {
        template: String,
        #[serde(default)]
        amount_field: Option<u8>,
        #[serde(default)]
        min_field: Option<u8>,
    },
    /// Out of the vault's Intents balance to its own address on another chain (hop.rs).
    Hop {},
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct FeedPrice {
    pub feed: String,
    #[serde(default)]
    pub quote: Option<String>,
}

/// Same fields and order as the vault's: a remote node's fingerprint is byte-identical.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub call: RemoteCall,
    #[serde(default)]
    pub out: Option<String>,
    #[serde(default)]
    pub max_out_per_in: Option<U128>,
    #[serde(default)]
    pub max_out: Option<U128>,
    #[serde(default)]
    pub price: Option<FeedPrice>,
}

#[near(serializers = [borsh, json])]
#[serde(tag = "chain", rename_all = "snake_case")]
#[derive(Clone, Debug)]
pub enum RemoteArgs {
    Evm { values: Vec<U128>, nonce: u64, max_fee: U128, max_tip: U128, gas: u64 },
    Sol { message: String },
    Hop {},
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub flow: u32,
    pub node: u16,
    pub amount: U128,
    pub min_out: U128,
    pub max_out: U128,
    pub expires_ns: Option<u64>,
    pub signed: bool,
    pub hash: String,
    pub revealed: Option<Node<Recipe>>,
}

/// A remote step about to start, carried through the brain's price read.
#[near(serializers = [borsh])]
pub struct Begin {
    flow_id: u32,
    node: u16,
    args: RemoteArgs,
    by_owner: bool,
    revealed: Option<Node<Recipe>>,
}

/// A token held where the brain can't read it as a NEAR balance: another chain,
/// or the vault's Intents account (`intents:<token>`, hop.rs). Funded on the owner's word.
pub fn is_remote(token: &str) -> bool {
    token.starts_with("evm:") || token.starts_with("sol:") || token.starts_with("intents:")
}

impl Remote {
    pub(crate) fn accepts(&self, input: &str) -> bool {
        let prefix = match self.call {
            RemoteCall::Evm { .. } => "evm:",
            RemoteCall::Sol { .. } => "sol:",
            // A hop spends an Intents balance and must say which chain it lands on;
            // its cap is the amount itself (hop.rs), its token checked at each step.
            RemoteCall::Hop {} => {
                let lands = |o: &str| (o.starts_with("sol:") || o.starts_with("evm:")) && o.len() <= 80;
                return input.starts_with("intents:") && input.len() <= 80 && self.out.as_deref().is_some_and(lands)
                    && self.price.as_ref().map_or(true, |p| !p.feed.is_empty());
            }
        };
        let collector = input == flow::NO_INPUT;
        let amount = match &self.call {
            RemoteCall::Evm { amount_leaf, .. } => amount_leaf.is_some(),
            RemoteCall::Sol { amount_field, .. } => amount_field.is_some(),
            RemoteCall::Hop {} => true,
        };
        let token_ok = |t: &str| t.starts_with(prefix) && t.len() <= 80;
        (collector || token_ok(input))
            && amount != collector
            && self.out.as_deref().map_or(true, |o| token_ok(o) && o != input)
            && (self.out.is_none() || if collector { self.max_out.is_some() } else { self.max_out_per_in.is_some() || self.max_out.is_some() })
            && self.price.as_ref().map_or(true, |p| !p.feed.is_empty())
    }

    /// The most a report may say came out (0: nothing comes out).
    fn out_cap(&self, amount: u128) -> u128 {
        if self.out.is_some() { self.cap(amount) } else { 0 }
    }

    fn cap(&self, amount: u128) -> u128 {
        let per_in = self.max_out_per_in.map(|m| crate::mul_div(amount, m.0, PRICE_SCALE));
        match (per_in, self.max_out) {
            (Some(a), Some(b)) => a.min(b.0),
            (Some(a), None) => a,
            (None, Some(b)) => b.0,
            (None, None) => 0,
        }
    }
}

impl Talk {
    /// Owner or keeper: run remote node `node` if its trigger has fired.
    pub(crate) fn step_remote(&mut self, sender: &AccountId, flow_id: u32, node: u16, args: RemoteArgs, reveal: Option<Node<Recipe>>, salt: Option<String>) -> Reply {
        let by_owner = *sender == self.vs.owner;
        require!(by_owner || Some(sender) == self.vs.keeper.as_ref(), "owner or keeper only");
        // No free work: a keeper run the vault can't pay for is refused before anything is signed.
        self.require_keeper_paid(by_owner);
        let f = self.flow(flow_id);
        let n = match (f.is_hidden(), reveal) {
            (false, None) => f.nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone(),
            (true, Some(n)) => {
                let print = f.hidden.get(node as usize).unwrap_or_else(|| env::panic_str("no such node"));
                require!(crate::hidden::fingerprint(&n, salt.as_deref().unwrap_or("")) == *print, "doesn't match the node's fingerprint");
                if let Err(e) = flow::validate_node(node, &n, &f.inputs) {
                    env::panic_str(&format!("bad node: {e:?}"));
                }
                n
            }
            (false, Some(_)) => env::panic_str("this flow is open: no reveal"),
            (true, None) => env::panic_str("a hidden flow's node comes with reveal and salt"),
        };
        require!(n.action.remote.is_some(), "not a remote node: use step");
        let need = if matches!(n.action.remote.as_ref().unwrap().call, RemoteCall::Hop {}) { HOP_NEED_TGAS } else { NEED_TGAS };
        require!(self.ctx.gas_left / 1_000_000_000_000 >= need, format!("attach at least {} Tgas for this step", need + crate::HANDLE_TGAS + crate::X_TGAS));
        self.need_after = need;
        self.check_node(&n);
        let revealed = self.flow(flow_id).is_hidden().then_some(n.clone());
        let feeds = self.remote_feeds(&n);
        if feeds.is_empty() {
            return self.remote_begin(flow_id, node, revealed, args, by_owner, &[]);
        }
        let views = self.feed_views(&feeds);
        self.read(views, Read::RemotePrices(Begin { flow_id, node, args, by_owner, revealed }), GAS_BUILD)
    }

    pub(crate) fn on_remote_prices(&mut self, b: Begin) -> Reply {
        let n = b.revealed.clone().unwrap_or_else(|| self.flow(b.flow_id).nodes[b.node as usize].clone());
        let feeds = self.remote_feeds(&n);
        let prices = self.named_prices(&feeds, 0);
        self.remote_begin(b.flow_id, b.node, b.revealed, b.args, b.by_owner, &prices)
    }

    /// The keeper (or owner) says how remote step `node` ended. Bounded, not measured.
    pub(crate) fn remote_report(&mut self, sender: &AccountId, flow_id: u32, node: u16, out: Option<U128>) -> Answer {
        let by_owner = *sender == self.vs.owner;
        require!(by_owner || Some(sender) == self.vs.keeper.as_ref(), "owner or keeper only");
        let p = self.pending(flow_id, node).unwrap_or_else(|| env::panic_str("no remote step pending"));
        let n = p.revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let result = match out {
            Some(o) => {
                require!(p.signed || by_owner, "not signed yet");
                require!(o.0 >= p.min_out.0, format!("refused: below the signed minimum {}: that transaction reverted", p.min_out.0));
                require!(o.0 <= p.max_out.0, format!("refused: over the node's cap {}", p.max_out.0));
                Ok(o.0)
            }
            // "It failed" gives the input back to be spent again. Nothing here can see the other
            // chain: had the transaction landed (before its deadline, too), a keeper's word
            // would spend the same input twice. Only the owner, who can look, says so.
            None => {
                require!(by_owner, "refused: only the owner may say a signed transaction failed (it may have landed: its input would be spent twice)");
                Err(p.amount.0)
            }
        };
        self.flow_mut(flow_id).ledger.finish_node(&n, node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.vs.remote.retain(|x| !(x.flow == flow_id && x.node == node));
        let (ev, k, v) = match result {
            Ok(o) => ("step", "out", o),
            Err(l) => ("step_failed", "returned", l),
        };
        self.emit(ev, &format!(r#""flow":{flow_id},"node":{node},"amount":"{}","{k}":"{v}","remote":true,"reported_by":"{}""#, p.amount.0, if by_owner { "owner" } else { "keeper" }));
        // The keeper ran it and it worked: its price, if it spent input or brought something
        // back (a collector that found nothing did no work worth paying for).
        let produced = p.amount.0 > 0 || matches!(result, Ok(o) if o > 0);
        if result.is_ok() && produced && !by_owner { self.pay_keeper(Answer::default()) } else { Answer::default() }
    }

    pub(crate) fn pending(&self, flow_id: u32, node: u16) -> Option<Pending> {
        self.vs.remote.iter().find(|p| p.flow == flow_id && p.node == node).cloned()
    }

    /// Why a remote node may not run, if it may not (called by `check_node`).
    pub(crate) fn remote_untrusted(&self, n: &Node<Recipe>) -> Option<String> {
        n.action.remote.as_ref()?;
        if !n.action.calls.is_empty() || n.action.out.is_some() || n.action.price.is_some() || n.action.market.is_some() {
            return Some("a remote node makes no NEAR calls".into());
        }
        None
    }

    fn remote_feeds(&self, n: &Node<Recipe>) -> Vec<String> {
        let r = n.action.remote.as_ref().unwrap();
        let mut out: Vec<String> = Vec::new();
        if matches!(n.when, Trigger::PriceBelow { .. } | Trigger::PriceAbove { .. }) {
            let p = r.price.as_ref().unwrap_or_else(|| env::panic_str("a price trigger needs remote.price (an oracle feed)"));
            out.push(p.feed.clone());
            out.extend(p.quote.clone());
        }
        let market = match &r.call {
            RemoteCall::Evm { template, .. } => crate::evm::market_feeds(&crate::evm::leaves(&self.evm_template_named(template))),
            // The node's template; a template riding along with a market field finds no price and is refused.
            RemoteCall::Sol { template, .. } => self.sol_feeds(&[template.as_str()]),
            // A hop has no market: what lands is the amount less the bridge's fee.
            RemoteCall::Hop {} => Vec::new(),
        };
        // A trigger may read a single-source feed; a market floor may not.
        self.require_confirmed(&market);
        out.extend(market);
        let mut seen = Vec::new();
        out.retain(|f| if seen.contains(f) { false } else { seen.push(f.clone()); true });
        out
    }

    fn remote_begin(&mut self, flow_id: u32, node: u16, revealed: Option<Node<Recipe>>, args: RemoteArgs, by_owner: bool, prices: &[(String, u128)]) -> Reply {
        let n = revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let r = n.action.remote.clone().unwrap();
        let price = match n.when {
            Trigger::PriceBelow { .. } | Trigger::PriceAbove { .. } => Some(trigger_price(r.price.as_ref().unwrap(), prices)),
            _ => None,
        };
        if let Some(p) = price {
            self.emit("remote_price", &format!(r#""flow":{flow_id},"node":{node},"price":"{p}""#));
        }
        let now = env::block_timestamp();
        let amount = self.flow_mut(flow_id).ledger.begin_node(&n, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        match (&r.call, args) {
            (RemoteCall::Evm { template, amount_leaf, min_leaf, deadline_leaf }, RemoteArgs::Evm { mut values, nonce, max_fee, max_tip, gas }) => {
                let cfg = self.vs.evm.clone().unwrap_or_else(|| env::panic_str("run evm_setup first"));
                let t = self.evm_template_named(template);
                let rules = crate::evm::leaves(&t);
                require!(values.len() == rules.len(), format!("one value per leaf ({})", rules.len()));
                let at = |i: &Option<u8>| i.map(|i| i as usize).filter(|i| *i < rules.len());
                if let Some(a) = at(amount_leaf) {
                    require!(matches!(rules[a], Arg::AtMost { .. }), "amount_leaf: an at_most leaf");
                    require!(evm_spent_token(&t, a, cfg.chain_id) == n.input, format!("the template doesn't spend {}", n.input));
                    values[a] = U128(amount);
                } else {
                    require!(amount_leaf.is_none() && amount == 0, "amount_leaf: no such leaf");
                }
                let deadline = crate::evm::deadline_s();
                let mut expires = rules.iter().any(|r| matches!(r, Arg::Deadline)).then_some((deadline + 1) * 1_000_000_000);
                if let Some(d) = at(deadline_leaf) {
                    require!(matches!(rules[d], Arg::AtMost { .. } | Arg::Deadline), "deadline_leaf: an at_most or deadline leaf");
                    values[d] = U128(deadline as u128);
                    expires = Some((deadline + 1) * 1_000_000_000);
                }
                let (tx, nums) = self.evm_prepare(&cfg, &t, values, nonce, max_fee, max_tip, gas, prices, !by_owner);
                let min = match at(min_leaf) {
                    Some(m) => {
                        require!(matches!(rules[m], Arg::Floor { .. } | Arg::Market { .. } | Arg::Share { .. }), "min_leaf: a floor, market or share leaf");
                        nums[m].unwrap_or(0)
                    }
                    None => 0,
                };
                let hash = format!("0x{}", hex::encode(tx.signing_hash()));
                self.remote_pending_start(flow_id, node, &r, amount, min, r.out_cap(amount), expires, hash, revealed);
                let (ans, carry) = crate::evm::evm_sign_call(&cfg.signer, &tx);
                let next = Next::RemoteEvmSigned { flow_id, node, tx: carry };
                self.charge_signature(ans).then("on_vault", near_sdk::borsh::to_vec(&next).unwrap(), GAS_SIGNED_CB).into()
            }
            (RemoteCall::Sol { template, amount_field, min_field }, RemoteArgs::Sol { message }) => {
                let (bytes, values) = self.sol_check_node(&message, template, &n.input, prices);
                if let Some(f) = amount_field {
                    require!(values.get(*f as usize) == Some(&amount), format!("refused: the message must spend exactly the node's {amount}"));
                }
                let min = min_field.and_then(|f| values.get(f as usize).copied()).unwrap_or(0);
                let hash = hex::encode(env::sha256(&bytes));
                self.remote_pending_start(flow_id, node, &r, amount, min, r.out_cap(amount), Some(now + SOL_TTL_S * 1_000_000_000), hash.clone(), revealed);
                let cfg = self.sol_cfg();
                let next = Next::RemoteSolSigned { flow_id, node, hash };
                self.charge_signature(crate::sol::sol_sign_call(&cfg.signer, &bytes)).then("on_vault", near_sdk::borsh::to_vec(&next).unwrap(), GAS_SIGNED_CB).into()
            }
            (RemoteCall::Hop {}, RemoteArgs::Hop {}) => {
                let token: AccountId = n.input.strip_prefix("intents:").and_then(|t| t.parse().ok()).unwrap_or_else(|| env::panic_str("a hop's input: intents:<token>"));
                let out = r.out.clone().unwrap();
                // What's reported must be on the chain the token lands on.
                let lands = match crate::hop::hop_chain_of(token.as_str()) {
                    Some((crate::hop::HopChain::Sol, _)) => "sol:".to_string(),
                    Some((crate::hop::HopChain::Evm, Some(id))) => format!("evm:{id}:"),
                    _ => env::panic_str("not a POA bridge token"),
                };
                require!(out.starts_with(&lands), format!("refused: {token} lands on {lands}, the node reports {out}"));
                let (ans, to, max_fee) = self.hop_call(&token, amount);
                // 1:1 (same decimals on both sides), tightened by the node's own caps if it has any.
                let max = if r.max_out_per_in.is_some() || r.max_out.is_some() { r.cap(amount).min(amount) } else { amount };
                let min = amount.saturating_sub(max_fee).min(max);
                // No deadline: once the token burned, only the bridge can bring it back, so only
                // the owner may call a hop failed (remote_report with no output).
                self.remote_pending_start(flow_id, node, &r, amount, min, max, None, format!("{}{to}", crate::hop::WITHDRAW_MEMO_PREFIX), revealed);
                let next = Next::RemoteHopSent { flow_id, node, amount: U128(amount) };
                ans.then("on_vault", near_sdk::borsh::to_vec(&next).unwrap(), crate::hop::GAS_HOP_SENT).into()
            }
            _ => env::panic_str("refused: args for another chain"),
        }
    }

    /// The verifier answered the hop's withdrawal: what it sent (ft_withdraw returns
    /// the amount the token took). All of it: in flight to the other chain, the
    /// report comes when it lands. Less (the token refused, the verifier refunded): failed.
    pub(crate) fn on_remote_hop_sent(&mut self, flow_id: u32, node: u16, amount: U128) -> Reply {
        let sent = crate::amount_result(0).unwrap_or(0);
        let ok = sent == amount.0;
        self.emit(if ok { "hop_sent" } else { "hop_failed" }, &format!(r#""flow":{flow_id},"node":{node},"amount":"{}","sent":"{sent}""#, amount.0));
        self.remote_signed(flow_id, node, ok);
        Answer::default().ret(&format!(r#""{sent}""#)).into()
    }

    #[allow(clippy::too_many_arguments)]
    fn remote_pending_start(&mut self, flow_id: u32, node: u16, r: &Remote, amount: u128, min: u128, max: u128, expires_ns: Option<u64>, hash: String, revealed: Option<Node<Recipe>>) {
        require!(min <= max || r.out.is_none(), format!("refused: the node's cap {max} is below its signed minimum {min}"));
        self.emit(
            "remote_step",
            &format!(r#""flow":{flow_id},"node":{node},"amount":"{amount}","min_out":"{min}","max_out":"{max}","hash":"{hash}","expires_ns":{}"#, expires_ns.map_or("null".into(), |e| e.to_string())),
        );
        require!(self.vs.remote.len() < 64, "too many remote steps pending");
        self.vs.remote.push(Pending { flow: flow_id, node, amount: U128(amount), min_out: U128(min), max_out: U128(max), expires_ns, signed: false, hash, revealed });
    }

    pub(crate) fn on_remote_evm_signed(&mut self, flow_id: u32, node: u16, tx: TxJson) -> Reply {
        let signed = self.evm_emit_signed(&tx);
        if signed.is_none() {
            self.evm_unreserve(&tx);
        }
        self.remote_signed(flow_id, node, signed.is_some());
        Answer::default().ret(&signed.unwrap_or_else(|| "null".into())).into()
    }

    pub(crate) fn on_remote_sol_signed(&mut self, flow_id: u32, node: u16, hash: String) -> Reply {
        let sig = self.sol_emit_signed(&hash);
        self.remote_signed(flow_id, node, sig.is_some());
        let ret = sig.map_or("null".into(), |s| format!(r#"{{"hash":"{hash}","signature":"{s}"}}"#));
        Answer::default().ret(&ret).into()
    }

    /// Signed: wait for the report. Not signed: nothing can land, the input goes back.
    fn remote_signed(&mut self, flow_id: u32, node: u16, ok: bool) {
        let Some(i) = self.vs.remote.iter().position(|p| p.flow == flow_id && p.node == node) else { return };
        if ok {
            self.vs.remote[i].signed = true;
            return;
        }
        let p = self.vs.remote.remove(i);
        let n = p.revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let _ = self.flow_mut(flow_id).ledger.finish_node(&n, node, Err(p.amount.0));
        self.emit("step_failed", &format!(r#""flow":{flow_id},"node":{node},"amount":"{}","returned":"{}","remote":true,"why":"not signed""#, p.amount.0, p.amount.0));
    }

    /// A Solana message of the owner's templates with exactly one `template`
    /// instruction (the node's), any others spending nothing. Spending is counted.
    fn sol_check_node(&mut self, message: &str, template: &str, input: &str, prices: &[(String, u128)]) -> (Vec<u8>, Vec<u128>) {
        let bytes = crate::sol::decode_message(message);
        let cfg = self.sol_cfg();
        let matched = self.sol_matched(&bytes, Some(prices));
        let mut mine = None;
        for m in &matched {
            let t = cfg.templates.iter().find(|t| t.name == m.template).unwrap();
            if m.template == template {
                require!(mine.is_none(), "refused: the node's template twice in one message");
                for s in &t.spends {
                    require!(input == flow::NO_INPUT || s.key == input, format!("the template spends {}, the node holds {input}", s.key));
                }
                mine = Some(m.values.clone());
            } else {
                require!(t.spends.is_empty(), format!("refused: {} rides along and spends", m.template));
            }
            for s in &t.spends {
                self.charge(&s.key, m.values[s.field as usize]);
            }
        }
        (bytes, mine.unwrap_or_else(|| env::panic_str(&format!("refused: no {template} instruction"))))
    }
}

fn evm_spent_token(t: &EvmTemplate, a: usize, chain_id: u64) -> String {
    if let Some(sp) = &t.spends {
        if sp.arg as usize == a {
            return format!("evm:{chain_id}:{}", sp.token);
        }
    }
    let value_leaf = crate::evm::leaves(t).len() - 1;
    match &t.value {
        Some(Arg::Same { of }) if *of as usize == a => format!("evm:{chain_id}:eth"),
        Some(Arg::AtMost { .. }) if value_leaf == a => format!("evm:{chain_id}:eth"),
        _ => String::new(),
    }
}

fn trigger_price(p: &FeedPrice, prices: &[(String, u128)]) -> u128 {
    let get = |f: &String| prices.iter().find(|(n, _)| n == f).map(|(_, v)| *v).unwrap_or_else(|| env::panic_str("refused: no price"));
    match &p.quote {
        Some(q) => crate::mul_div(get(&p.feed), PRICE_SCALE, get(q)),
        None => get(&p.feed).saturating_mul(1_000_000),
    }
}

// The full vault's tests, carried over unchanged when it was removed.
#[cfg(test)]
mod tests {
    use super::*;

    fn evm(out: Option<&str>, per_in: Option<u128>, max: Option<u128>, amount: bool) -> Remote {
        Remote {
            call: RemoteCall::Evm { template: "t".into(), amount_leaf: amount.then_some(1), min_leaf: None, deadline_leaf: None },
            out: out.map(String::from),
            max_out_per_in: per_in.map(U128),
            max_out: max.map(U128),
            price: None,
        }
    }

    #[test]
    fn remote_nodes_take_remote_tokens_and_must_cap_what_is_reported() {
        let nvda = "evm:4663:0xd060";
        assert!(evm(Some("evm:4663:eth"), Some(1), None, true).accepts(nvda));
        // Uncapped output, a NEAR token, the same token out, the other chain's token: refused.
        assert!(!evm(Some("evm:4663:eth"), None, None, true).accepts(nvda));
        assert!(!evm(Some("evm:4663:eth"), Some(1), None, true).accepts("usdc.near"));
        assert!(!evm(Some(nvda), Some(1), None, true).accepts(nvda));
        assert!(!evm(Some("sol:So111"), Some(1), None, true).accepts(nvda));
        // A collector spends nothing (no amount leaf) and needs a per-step cap.
        assert!(evm(Some("evm:4663:eth"), None, Some(5), false).accepts(flow::NO_INPUT));
        assert!(!evm(Some("evm:4663:eth"), Some(5), None, false).accepts(flow::NO_INPUT));
        assert!(!evm(Some("evm:4663:eth"), None, Some(5), true).accepts(flow::NO_INPUT));
        assert!(!evm(Some("evm:4663:eth"), Some(1), None, false).accepts(nvda), "a spender must say where its input goes");
    }

    #[test]
    fn the_cap_is_the_tighter_of_per_input_and_per_step() {
        // 0.2 ETH per NVDA (both 18 decimals): 0.3 NVDA in -> at most 0.06 ETH.
        let per_in = 2 * PRICE_SCALE / 10;
        let nvda_in = 300_000_000_000_000_000u128;
        assert_eq!(evm(Some("evm:1:eth"), Some(per_in), None, true).cap(nvda_in), 60_000_000_000_000_000);
        assert_eq!(evm(Some("evm:1:eth"), Some(per_in), Some(1_000), true).cap(nvda_in), 1_000);
        assert_eq!(evm(Some("evm:1:eth"), None, Some(7), false).cap(0), 7);
        assert_eq!(evm(Some("evm:1:eth"), Some(per_in), None, true).cap(0), 0, "nothing in, nothing out");
    }

    #[test]
    fn trigger_prices_come_in_1e24_like_every_trigger() {
        const E18: u128 = 1_000_000_000_000_000_000;
        // NVDA $180, ETH $2,400: NVDA in USD = 180e24, in ETH = 0.075e24.
        let prices = vec![("nvda".to_string(), 180 * E18), ("eth".to_string(), 2_400 * E18)];
        assert_eq!(trigger_price(&FeedPrice { feed: "nvda".into(), quote: None }, &prices), 180 * PRICE_SCALE);
        assert_eq!(trigger_price(&FeedPrice { feed: "nvda".into(), quote: Some("eth".into()) }, &prices), 75 * PRICE_SCALE / 1000);
    }
}
