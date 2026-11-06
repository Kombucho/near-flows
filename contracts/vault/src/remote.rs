//! Remote nodes: a flow node whose action is a transaction on another chain
//! (an EVM template, a Solana template) instead of NEAR calls.
//!
//! The vault still decides everything it can decide itself:
//! - the trigger: time triggers from the block clock, price triggers from the
//!   oracle (Pyth, oracle.rs) read by the vault, never a number the keeper sends;
//! - the amount: the node's own input (its inbox, or its DCA chunk) goes into the
//!   template's amount leaf; the keeper's number for that leaf is ignored;
//! - the minimum: the template's floor / market rule, which the signed
//!   transaction itself enforces (it reverts below it);
//! - the deadline: an EVM node can carry one, filled by the vault (now + 5 min).
//!
//! What the vault CANNOT do is see a balance on another chain. So a remote
//! node's output is KEEPER-REPORTED (`remote_report`), and only accepted within
//! bounds: at least the minimum the vault signed (less means the transaction
//! reverted), at most the node's cap (`max_out_per_in` x input, and/or
//! `max_out`). It is capped and floored, not measured. Within those bounds a
//! lying keeper can misroute this node's output between the flow's later nodes
//! (which then sign transactions that revert for lack of funds, or spend money
//! the vault holds there for other flows), never more than the caps and the
//! spending limits allow, and never to an address the templates don't name.
//!
//! A step is in two parts: `step_remote` (trigger checked, input taken, transaction
//! built and signed: the flow is busy) and `remote_report` (landed with this
//! output: routed on; or failed: the input goes back to the node). A keeper may
//! report a failure only once the signed transaction can no longer land (its
//! deadline passed, or a Solana blockhash's ~1 minute life); before that, or
//! for an EVM node without a deadline, only the owner can.
//!
//! Tokens on other chains are named like spending limits: "evm:<chain id>:<0x token>"
//! ("evm:<chain id>:eth" for the native coin) and "sol:<mint or key>". The owner
//! funds a remote entry node with what they put at the vault's remote address:
//! the vault takes their word for it (it's their money and their claim).

// Some helpers serve only the EVM or the Solana half: a build without one leaves them unused.
#![cfg_attr(not(all(feature = "evm", feature = "sol")), allow(dead_code, unused_variables))]

use flow::{Node, Trigger};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, Gas, Promise};

use crate::{Recipe, Vault, VaultExt, PRICE_SCALE};

/// A signed EVM transaction's deadline, from the block that signs it.
pub const EVM_TTL_S: u64 = 300;
/// A Solana transaction lands within ~150 slots of its blockhash (~1 minute); 3 minutes to be safe.
pub const SOL_TTL_S: u64 = 180;
const GAS_BUILD: Gas = Gas::from_tgas(120);
const GAS_SIGNED_CB: Gas = Gas::from_tgas(20);
/// What `step_remote` needs: oracle reads, the build + sign callback, its own callback.
pub const STEP_REMOTE_TGAS: u64 = 200;

/// What a remote node signs.
#[near(serializers = [borsh, json])]
#[serde(tag = "chain", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteCall {
    /// An EVM template (evm.rs) on the vault's EVM chain. Leaves are numbered as in
    /// evm_call: `amount_leaf` gets the node's input (an at_most leaf), `min_leaf` is
    /// the floor or market leaf whose number is the signed minimum output,
    /// `deadline_leaf` an at_most leaf the vault fills with now + 5 minutes.
    Evm {
        template: String,
        #[serde(default)]
        amount_leaf: Option<u8>,
        #[serde(default)]
        min_leaf: Option<u8>,
        #[serde(default)]
        deadline_leaf: Option<u8>,
    },
    /// A Solana transaction made of the owner's templates (soltemplates.rs), with
    /// exactly one instruction of `template`: its field `amount_field` must equal the
    /// node's input, its field `min_field` is the signed minimum output.
    Sol {
        template: String,
        #[serde(default)]
        amount_field: Option<u8>,
        #[serde(default)]
        min_field: Option<u8>,
    },
}

/// A price the vault reads from the oracle: `feed` in units of `quote`, or in USD
/// without one; scaled 1e24 like every trigger price (NVDA at $180 = 180e24).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct FeedPrice {
    pub feed: String,
    #[serde(default)]
    pub quote: Option<String>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub call: RemoteCall,
    /// What lands at the vault's remote address, e.g. "evm:4663:eth".
    #[serde(default)]
    pub out: Option<String>,
    /// The most output per unit of input a keeper may report (1e24): the node's cap.
    #[serde(default)]
    pub max_out_per_in: Option<U128>,
    /// The most output per step a keeper may report (a collector's only cap).
    #[serde(default)]
    pub max_out: Option<U128>,
    /// Where price triggers read their price.
    #[serde(default)]
    pub price: Option<FeedPrice>,
}

/// What the keeper brings to a remote step: the template's other numbers and the
/// gas terms (EVM), or the message it built (Solana).
#[near(serializers = [json])]
#[serde(tag = "chain", rename_all = "snake_case")]
#[derive(Clone, Debug)]
pub enum RemoteArgs {
    Evm { values: Vec<U128>, nonce: u64, max_fee: U128, max_tip: U128, gas: u64 },
    Sol { message: String },
}

/// A remote step waiting for its report.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub amount: U128,
    /// The least output the signed transaction accepts.
    pub min_out: U128,
    /// The most output that may be reported.
    pub max_out: U128,
    /// After this, the signed transaction can't land any more (None: no deadline).
    pub expires_ns: Option<u64>,
    pub signed: bool,
    /// The hash the vault signed (EVM signing hash / sha256 of the Solana message).
    pub hash: String,
    pub revealed: Option<Node<Recipe>>,
}

pub fn is_remote(token: &str) -> bool {
    token.starts_with("evm:") || token.starts_with("sol:")
}

impl Remote {
    /// The shape checks of a saved node (flow::validate calls them through Recipe::accepts).
    pub(crate) fn accepts(&self, input: &str) -> bool {
        let prefix = match self.call {
            RemoteCall::Evm { .. } => "evm:",
            RemoteCall::Sol { .. } => "sol:",
        };
        let collector = input == flow::NO_INPUT;
        let amount = match &self.call {
            RemoteCall::Evm { amount_leaf, .. } => amount_leaf.is_some(),
            RemoteCall::Sol { amount_field, .. } => amount_field.is_some(),
        };
        let token_ok = |t: &str| t.starts_with(prefix) && t.len() <= 80;
        (collector || token_ok(input))
            // A spender says where its input goes; a collector spends nothing.
            && amount != collector
            && self.out.as_deref().map_or(true, |o| token_ok(o) && o != input)
            // Anything reported must be capped: per unit of input, or per step for a collector.
            && (self.out.is_none() || if collector { self.max_out.is_some() } else { self.max_out_per_in.is_some() || self.max_out.is_some() })
            && self.price.as_ref().map_or(true, |p| !p.feed.is_empty())
    }

    /// The most a keeper may report for `amount` in.
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

#[near]
impl Vault {
    /// Run remote node `node` of flow `flow_id`, if its trigger has fired: build its
    /// transaction from `args` and get it signed (event `evm_signed` / `sol_signed`).
    /// A hidden flow's node comes with `reveal` and `salt`. Owner or keeper.
    pub fn step_remote(&mut self, flow_id: u32, node: u16, args: RemoteArgs, reveal: Option<Node<Recipe>>, salt: Option<String>) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        require!(env::prepaid_gas().as_tgas() >= STEP_REMOTE_TGAS, format!("attach at least {STEP_REMOTE_TGAS} Tgas for this step"));
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
        if let Some(why) = self.untrusted(&n) {
            env::panic_str(&format!("untrusted: {why}"));
        }
        let revealed = self.flow(flow_id).is_hidden().then_some(n.clone());
        let feeds = self.remote_feeds(&n);
        if feeds.is_empty() {
            return self.remote_begin(flow_id, node, revealed, args, by_owner, &[]);
        }
        self.oracle_fetch(&feeds).then(Self::ext(env::current_account_id()).with_static_gas(GAS_BUILD).with_unused_gas_weight(1).on_remote_prices(flow_id, node, args, by_owner, revealed))
    }

    #[private]
    pub fn on_remote_prices(&mut self, flow_id: u32, node: u16, args: RemoteArgs, by_owner: bool, revealed: Option<Node<Recipe>>) -> Promise {
        let n = revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let feeds = self.remote_feeds(&n);
        let prices: Vec<(String, u128)> = feeds.iter().cloned().zip(self.oracle_prices(&feeds, 0)).collect();
        self.remote_begin(flow_id, node, revealed, args, by_owner, &prices)
    }

    /// The keeper (or owner) says how remote step `node` ended: `out` = it landed and
    /// this much arrived at the vault's remote address; None = it failed. Bounded,
    /// not measured: see the top of this file.
    pub fn remote_report(&mut self, flow_id: u32, node: u16, out: Option<U128>) {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        let p = self.pending(flow_id, node).unwrap_or_else(|| env::panic_str("no remote step pending"));
        let n = p.revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let now = env::block_timestamp();
        let result = match out {
            Some(o) => {
                require!(p.signed || by_owner, "not signed yet");
                require!(o.0 >= p.min_out.0, format!("refused: below the signed minimum {}: that transaction reverted", p.min_out.0));
                require!(o.0 <= p.max_out.0, format!("refused: over the node's cap {}", p.max_out.0));
                Ok(o.0)
            }
            None => {
                require!(by_owner || p.expires_ns.map_or(false, |e| now >= e), "refused: the signed transaction may still land; the owner can settle it, or wait for its deadline");
                Err(p.amount.0)
            }
        };
        let f = self.flow_mut(flow_id);
        f.ledger.finish_node(&n, node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        env::storage_remove(&key(flow_id, node));
        let (ev, k, v) = match result {
            Ok(o) => ("step", "out", o),
            Err(l) => ("step_failed", "returned", l),
        };
        self.emit(ev, &format!(r#"{{"flow":{flow_id},"node":{node},"amount":"{}","{k}":"{v}","remote":true,"reported_by":"{}"}}"#, p.amount.0, if by_owner { "owner" } else { "keeper" }));
    }

    pub fn get_remote_pending(&self, flow_id: u32, node: u16) -> Option<Pending> {
        self.pending(flow_id, node)
    }
}

#[cfg(feature = "evm")]
#[near]
impl Vault {
    #[private]
    pub fn on_remote_evm_signed(&mut self, flow_id: u32, node: u16, tx: crate::evm::TxJson) {
        let ok = self.evm_emit_signed(&tx);
        self.remote_signed(flow_id, node, ok);
    }
}

#[cfg(feature = "sol")]
#[near]
impl Vault {
    #[private]
    pub fn on_remote_sol_signed(&mut self, flow_id: u32, node: u16, hash: String) {
        let raw = env::promise_result_checked(0, 4096).ok();
        let sig = raw.as_deref().and_then(crate::signature_from).map(hex::encode);
        match &sig {
            Some(s) => self.emit("sol_signed", &format!(r#"{{"hash":"{hash}","signature":"{s}"}}"#)),
            None => self.emit("sol_sign_failed", &format!(r#"{{"hash":"{hash}"}}"#)),
        }
        self.remote_signed(flow_id, node, sig.is_some());
    }
}

/// Pending steps live under their own storage keys, outside the vault's state struct.
fn key(flow_id: u32, node: u16) -> Vec<u8> {
    format!("rp:{flow_id}:{node}").into_bytes()
}

impl Vault {
    fn pending(&self, flow_id: u32, node: u16) -> Option<Pending> {
        env::storage_read(&key(flow_id, node)).and_then(|b| near_sdk::borsh::from_slice(&b).ok())
    }

    fn put_pending(&mut self, flow_id: u32, node: u16, p: &Pending) {
        env::storage_write(&key(flow_id, node), &near_sdk::borsh::to_vec(p).unwrap());
    }

    /// Why a remote node may not run, if it may not (called by `untrusted`).
    pub(crate) fn remote_untrusted(&self, n: &Node<Recipe>) -> Option<String> {
        let r = n.action.remote.as_ref()?;
        if !n.action.calls.is_empty() || n.action.out.is_some() || n.action.price.is_some() || n.action.market.is_some() {
            return Some("a remote node makes no NEAR calls".into());
        }
        match &r.call {
            RemoteCall::Evm { .. } if cfg!(not(feature = "evm")) => Some("this vault has no EVM".into()),
            RemoteCall::Sol { .. } if cfg!(not(feature = "sol")) => Some("this vault has no Solana".into()),
            _ => None,
        }
    }

    /// The feeds a remote step reads: its trigger price, and its template's market floors.
    fn remote_feeds(&self, n: &Node<Recipe>) -> Vec<String> {
        let r = n.action.remote.as_ref().unwrap();
        let mut out: Vec<String> = Vec::new();
        if matches!(n.when, Trigger::PriceBelow { .. } | Trigger::PriceAbove { .. }) {
            let p = r.price.as_ref().unwrap_or_else(|| env::panic_str("a price trigger needs remote.price (an oracle feed)"));
            out.push(p.feed.clone());
            out.extend(p.quote.clone());
        }
        #[cfg(feature = "evm")]
        if let RemoteCall::Evm { template, .. } = &r.call {
            out.extend(crate::evm::market_feeds(&crate::evm::leaves(&self.evm_template_named(template))));
        }
        let mut seen = Vec::new();
        out.retain(|f| if seen.contains(f) { false } else { seen.push(f.clone()); true });
        out
    }

    /// Trigger checked, input taken from the node, transaction built and sent to the signer.
    fn remote_begin(&mut self, flow_id: u32, node: u16, revealed: Option<Node<Recipe>>, args: RemoteArgs, by_owner: bool, prices: &[(String, u128)]) -> Promise {
        let n = revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let r = n.action.remote.clone().unwrap();
        let price = match n.when {
            Trigger::PriceBelow { .. } | Trigger::PriceAbove { .. } => Some(trigger_price(r.price.as_ref().unwrap(), prices)),
            _ => None,
        };
        if let Some(p) = price {
            self.emit("remote_price", &format!(r#"{{"flow":{flow_id},"node":{node},"price":"{p}"}}"#));
        }
        let now = env::block_timestamp();
        let amount = self.flow_mut(flow_id).ledger.begin_node(&n, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        // Everything below runs in this same receipt: a refusal undoes the ledger too.
        match (&r.call, args) {
            #[cfg(feature = "evm")]
            (RemoteCall::Evm { template, amount_leaf, min_leaf, deadline_leaf }, RemoteArgs::Evm { mut values, nonce, max_fee, max_tip, gas }) => {
                use crate::evm::Arg;
                let cfg = self.evm.clone().unwrap_or_else(|| env::panic_str("run evm_setup first"));
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
                let mut expires = None;
                if let Some(d) = at(deadline_leaf) {
                    require!(matches!(rules[d], Arg::AtMost { .. }), "deadline_leaf: an at_most leaf");
                    let deadline = now / 1_000_000_000 + EVM_TTL_S;
                    values[d] = U128(deadline as u128);
                    // A block at the deadline second can still include it: a failure counts from the second after.
                    expires = Some((deadline + 1) * 1_000_000_000);
                }
                let (tx, nums) = self.evm_prepare(&cfg, &t, values, nonce, max_fee, max_tip, gas, prices);
                let min = match at(min_leaf) {
                    Some(m) => {
                        require!(matches!(rules[m], Arg::Floor { .. } | Arg::Market { .. }), "min_leaf: a floor or market leaf");
                        nums[m].unwrap_or(0)
                    }
                    None => 0,
                };
                let hash = format!("0x{}", hex::encode(tx.signing_hash()));
                self.remote_pending_start(flow_id, node, &r, amount, min, expires, hash, revealed);
                let (sign, carry) = crate::evm::evm_sign_request(cfg.signer, &tx);
                sign.then(Self::ext(env::current_account_id()).with_static_gas(GAS_SIGNED_CB).on_remote_evm_signed(flow_id, node, carry))
            }
            #[cfg(feature = "sol")]
            (RemoteCall::Sol { template, amount_field, min_field }, RemoteArgs::Sol { message }) => {
                let (bytes, values) = self.sol_check_node(&message, template, &n.input);
                if let Some(f) = amount_field {
                    require!(values.get(*f as usize) == Some(&amount), format!("refused: the message must spend exactly the node's {amount}"));
                }
                let min = min_field.and_then(|f| values.get(f as usize).copied()).unwrap_or(0);
                let hash = hex::encode(env::sha256(&bytes));
                let expires = Some(now + SOL_TTL_S * 1_000_000_000);
                self.remote_pending_start(flow_id, node, &r, amount, min, expires, hash.clone(), revealed);
                let cfg = self.sol.clone().unwrap();
                let args = near_sdk::serde_json::json!({"request": {"path": crate::sol::SOL_PATH, "payload_v2": {"Eddsa": hex::encode(&bytes)}, "domain_id": crate::sol::EDDSA_DOMAIN}});
                Promise::new(cfg.signer)
                    .function_call("sign".to_string(), args.to_string().into_bytes(), near_sdk::NearToken::from_yoctonear(1), crate::sol::GAS_SIGN)
                    .then(Self::ext(env::current_account_id()).with_static_gas(GAS_SIGNED_CB).on_remote_sol_signed(flow_id, node, hash))
            }
            _ => env::panic_str("refused: args for another chain (or a chain this vault doesn't have)"),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn remote_pending_start(&mut self, flow_id: u32, node: u16, r: &Remote, amount: u128, min: u128, expires_ns: Option<u64>, hash: String, revealed: Option<Node<Recipe>>) {
        let max = if r.out.is_some() { r.cap(amount) } else { 0 };
        require!(min <= max || r.out.is_none(), format!("refused: the node's cap {max} is below its signed minimum {min}"));
        self.emit(
            "remote_step",
            &format!(r#"{{"flow":{flow_id},"node":{node},"amount":"{amount}","min_out":"{min}","max_out":"{max}","hash":"{hash}","expires_ns":{}}}"#, expires_ns.map_or("null".into(), |e| e.to_string())),
        );
        self.put_pending(flow_id, node, &Pending { amount: U128(amount), min_out: U128(min), max_out: U128(max), expires_ns, signed: false, hash, revealed });
    }

    /// The signer answered. Signed: wait for the report. Not signed: nothing can land, the input goes back.
    fn remote_signed(&mut self, flow_id: u32, node: u16, ok: bool) {
        let Some(mut p) = self.pending(flow_id, node) else { return };
        if ok {
            p.signed = true;
            self.put_pending(flow_id, node, &p);
            return;
        }
        let n = p.revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone());
        let _ = self.flow_mut(flow_id).ledger.finish_node(&n, node, Err(p.amount.0));
        env::storage_remove(&key(flow_id, node));
        self.emit("step_failed", &format!(r#"{{"flow":{flow_id},"node":{node},"amount":"{}","returned":"{}","remote":true,"why":"not signed"}}"#, p.amount.0, p.amount.0));
    }

    #[cfg(feature = "evm")]
    fn evm_template_named(&self, name: &str) -> crate::evm::EvmTemplate {
        let cfg = self.evm.as_ref().unwrap_or_else(|| env::panic_str("run evm_setup first"));
        cfg.templates.iter().find(|t| t.name == name).cloned().unwrap_or_else(|| env::panic_str("refused: no such template"))
    }

    /// Check a Solana message against the owner's templates (as sol_sign_tx does) and
    /// that it is this node's: exactly one instruction of `template`, any others
    /// spending nothing. Spending is counted. Returns the bytes and that instruction's fields.
    #[cfg(feature = "sol")]
    fn sol_check_node(&mut self, message: &str, template: &str, input: &str) -> (Vec<u8>, Vec<u128>) {
        use crate::soltemplates::SolTemplate;
        let bytes = near_sdk::base64::Engine::decode(&near_sdk::base64::engine::general_purpose::STANDARD, message).unwrap_or_else(|_| env::panic_str("message: base64"));
        require!(bytes.len() <= 1232, "a Solana message fits in 1232 bytes");
        let cfg = self.sol.clone().unwrap_or_else(|| env::panic_str("run sol_setup first"));
        let address = cfg.address.clone().unwrap_or_else(|| env::panic_str("the vault's Solana address isn't known yet"));
        let policy = solguard::template::TxPolicy {
            authority: solguard::pubkey(&address),
            owner: cfg.owner_wallet.as_deref().map(solguard::pubkey),
            max_cu_price: cfg.max_cu_price,
            max_cu_limit: cfg.max_cu_limit,
        };
        let guards: Vec<solguard::template::Template> = cfg.templates.iter().map(SolTemplate::to_guard).collect();
        let matched = solguard::template::check_tx(&bytes, &guards, &policy).unwrap_or_else(|e| env::panic_str(&format!("refused: {e:?}")));
        let mut mine = None;
        for m in &matched {
            let t = cfg.templates.iter().find(|t| t.name == m.template).unwrap();
            if m.template == template {
                require!(mine.is_none(), "refused: the node's template twice in one message");
                // Its input must be what the template spends (when it spends anything).
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

/// The token an EVM template's leaf `a` spends: its `spends` token, or the chain's
/// coin when the attached value is that leaf (or the same number).
#[cfg(feature = "evm")]
fn evm_spent_token(t: &crate::evm::EvmTemplate, a: usize, chain_id: u64) -> String {
    use crate::evm::Arg;
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

/// `p` scaled 1e24: feed / quote, or the feed in USD. Prices come from `oracle_prices` (USD, 1e18).
fn trigger_price(p: &FeedPrice, prices: &[(String, u128)]) -> u128 {
    let get = |f: &String| prices.iter().find(|(n, _)| n == f).map(|(_, v)| *v).unwrap_or_else(|| env::panic_str("refused: no price"));
    match &p.quote {
        Some(q) => crate::mul_div(get(&p.feed), PRICE_SCALE, get(q)),
        None => get(&p.feed).saturating_mul(1_000_000),
    }
}

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
