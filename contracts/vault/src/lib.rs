//! A user's own vault: it holds their money and runs their flows.
//!
//! Every user gets their own vault account, so a loan on Rhea, a LiNEAR stake
//! or a dip buy made by a flow belongs to THAT vault only: one user's flow can
//! never touch another user's money or health factor.
//!
//! A node's action is a RECIPE: one or more contract calls, with the node's
//! input spent by exactly one of them (as a token transfer, or as attached
//! NEAR). The Rhea, LiNEAR, market and Bazaar nodes are ready-made recipes;
//! an agent writes new ones the same way. Recipes are plain data anyone can
//! read: which contract, which method, which arguments.
//!
//! Who can do what:
//! - The owner: save, fund, run, stop and delete flows; withdraw; pick a keeper.
//! - The keeper: run a step whose trigger has fired. Nothing else.
//! - Money leaves the vault only through the owner's own recipes or to the owner.
//!
//! Balances are never kept in a separate ledger that could drift: the vault's
//! free balance of a token is what it really holds, minus what its flows hold.
//! A step's output is measured as the vault's balance after minus before
//! (one step at a time per vault, so the difference is that step's).

mod limits;
mod step;
#[cfg(test)]
mod tests;

use flow::{Action, Ledger, Node, Token};
use near_contract_standards::fungible_token::core::ext_ft_core;
use near_contract_standards::fungible_token::receiver::FungibleTokenReceiver;
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue};

pub use limits::{Limit, PendingLimit};
pub use step::StepCtx;

pub const MAX_FLOWS: usize = 16;
pub const MAX_CALLS: usize = 4;
/// Most gas a recipe's calls may ask for in total (the rest pays for our callbacks).
pub const MAX_RECIPE_TGAS: u64 = 200;
/// NEAR the vault always keeps for its own storage and fees.
pub const RESERVE: NearToken = NearToken::from_millinear(200);
/// A step stuck this long (e.g. a callback ran out of gas) can be cleared by the owner.
pub const STUCK_NS: u64 = 10 * 60 * 1_000_000_000;
pub const PRICE_SCALE: u128 = 1_000_000_000_000_000_000_000_000;
pub const NEAR: &str = "near";

const GAS_VIEW: Gas = Gas::from_tgas(10);
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);

/// What a call attaches.
#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Deposit {
    None,
    OneYocto,
    /// The node's input, when the input is native NEAR (e.g. LiNEAR deposit_and_stake).
    Input,
    Fixed { yocto: U128 },
}

/// One contract call in a recipe. In `args` and `msg`, these are filled in:
/// {amount} the input amount, {min_out} the least output accepted,
/// {scaled} the input amount times the recipe's `scale_per_in` (e.g. how much
/// to borrow against this much collateral), {owner} the vault's owner,
/// {vault} the vault itself.
#[near(serializers = [borsh, json])]
#[serde(tag = "call", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    /// Send the node's input token to `receiver` with ft_transfer_call (Rhea swaps, lending supply, market buys).
    Ft { receiver: AccountId, msg: String, tgas: u64 },
    /// Call any method.
    Fn { contract: AccountId, method: String, args: String, deposit: Deposit, tgas: u64 },
}

impl Call {
    fn spends_input(&self) -> bool {
        matches!(self, Call::Ft { .. } | Call::Fn { deposit: Deposit::Input, .. })
    }
    fn tgas(&self) -> u64 {
        match self {
            Call::Ft { tgas, .. } | Call::Fn { tgas, .. } => *tgas,
        }
    }
}

/// Where a price trigger reads its price: a view returning an amount (U128 string)
/// for `per` units in, e.g. Rhea `get_return`. Price = result * 1e24 / per.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceSource {
    pub contract: AccountId,
    pub method: String,
    pub args: String,
    pub per: U128,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Recipe {
    /// What the canvas shows, e.g. "Swap on Rhea".
    pub label: String,
    pub calls: Vec<Call>,
    /// The token it hands out, if any.
    pub out: Option<AccountId>,
    /// Least output per unit of input (scaled 1e24); fills {min_out}, and a step
    /// that gets less pauses its node.
    pub min_out_per_in: Option<U128>,
    /// Needed by price triggers.
    pub price: Option<PriceSource>,
    /// Fills {scaled}: amount * scale_per_in / 1e24.
    #[serde(default)]
    pub scale_per_in: Option<U128>,
}

impl Action for Recipe {
    fn output(&self, _input: &Token) -> Option<Token> {
        self.out.as_ref().map(|a| a.to_string())
    }
    fn accepts(&self, input: &Token) -> bool {
        let spenders: Vec<&Call> = self.calls.iter().filter(|c| c.spends_input()).collect();
        let [spender] = spenders.as_slice() else { return false };
        let right_kind = match spender {
            Call::Ft { .. } => input != NEAR,
            Call::Fn { .. } => input == NEAR,
        };
        let total_gas: u64 = self.calls.iter().map(Call::tgas).sum();
        right_kind
            && !self.calls.is_empty()
            && self.calls.len() <= MAX_CALLS
            && total_gas <= MAX_RECIPE_TGAS
            && self.calls.iter().all(|c| c.tgas() >= 1)
            && self.out.as_ref().map_or(true, |o| o.as_str() != input && o.as_str() != NEAR)
            && self.label.len() <= 60
    }
}

/// A trusted contract: recipes may send it tokens (ft_transfer_call), use it as
/// a token, and call exactly these methods on it.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Trust {
    pub contract: AccountId,
    #[serde(default)]
    pub methods: Vec<String>,
}

/// Methods that move tokens or NEAR out. Never callable directly by a recipe,
/// trusted or not: tokens leave only through the step's own metered transfer,
/// or by the owner's withdraw.
pub const NEVER_CALL: &[&str] = &[
    "ft_transfer", "ft_transfer_call", "nft_transfer", "nft_transfer_call", "mt_transfer", "mt_transfer_call",
    "mt_batch_transfer", "mt_batch_transfer_call", "near_withdraw", "storage_withdraw", "storage_unregister",
    "nft_approve", "ft_approve", "mt_approve",
];

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct FlowRec {
    pub name: String,
    pub nodes: Vec<Node<Recipe>>,
    pub ledger: Ledger,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    pub owner: AccountId,
    pub keeper: Option<AccountId>,
    pub flows: Vec<Option<FlowRec>>,
    /// The step in flight, if any (one per vault).
    pub busy: Option<StepCtx>,
    /// Contracts recipes may touch, and the methods they may call on each.
    /// The vault itself never.
    pub trusted: Vec<Trust>,
    /// Per-token spending limits on what steps may spend, and loosenings waiting out their delay.
    pub limits: Vec<Limit>,
    pub pending: Vec<PendingLimit>,
}

#[near]
impl Vault {
    #[init]
    pub fn new(owner: AccountId, keeper: Option<AccountId>) -> Self {
        Self { owner, keeper, flows: Vec::new(), busy: None, trusted: Vec::new(), limits: Vec::new(), pending: Vec::new() }
    }

    // ---------- owner ----------

    pub fn set_keeper(&mut self, keeper: Option<AccountId>) {
        self.assert_owner();
        self.keeper = keeper;
    }

    /// Trust contracts (and the methods recipes may call on them), or stop
    /// trusting them. Trusting a contract again replaces its method list.
    pub fn trust(&mut self, add: Vec<Trust>, remove: Vec<AccountId>) {
        self.assert_owner();
        self.trusted.retain(|t| !remove.contains(&t.contract));
        for t in add {
            require!(t.contract != env::current_account_id(), "a vault never trusts itself");
            for m in &t.methods {
                require!(!NEVER_CALL.contains(&m.as_str()), format!("{m} moves funds: recipes can never call it"));
                require!(!m.is_empty() && m.len() <= 64, "bad method");
            }
            require!(t.methods.len() <= 16, "too many methods");
            self.trusted.retain(|x| x.contract != t.contract);
            self.trusted.push(t);
        }
        require!(self.trusted.len() <= 64, "too many trusted contracts");
    }

    /// Save a flow. Returns its id. Every contract its recipes touch must be trusted.
    pub fn save_flow(&mut self, name: String, nodes: Vec<Node<Recipe>>) -> u32 {
        self.assert_owner();
        require!(name.len() <= 60, "name too long");
        if let Err(e) = flow::validate(&nodes) {
            env::panic_str(&format!("bad flow: {e:?}"));
        }
        for n in &nodes {
            if let Some(why) = self.untrusted(n) {
                env::panic_str(&format!("untrusted: {why}"));
            }
        }
        let rec = FlowRec { name, ledger: Ledger::new(nodes.len()), nodes };
        if let Some(i) = self.flows.iter().position(Option::is_none) {
            self.flows[i] = Some(rec);
            return i as u32;
        }
        require!(self.flows.len() < MAX_FLOWS, "too many flows");
        self.flows.push(Some(rec));
        (self.flows.len() - 1) as u32
    }

    /// Delete an empty flow.
    pub fn delete_flow(&mut self, flow_id: u32) {
        self.assert_owner();
        self.assert_idle();
        let f = self.flow(flow_id);
        require!(f.ledger.holdings(&f.nodes).is_empty(), "drain the flow first");
        self.flows[flow_id as usize] = None;
    }

    /// Put `amount` of the vault's free balance into an entry node.
    /// Native NEAR is checked right away; a token needs the vault's balance first.
    pub fn fund(&mut self, flow_id: u32, node: u16, amount: U128) -> PromiseOrValue<()> {
        self.assert_owner();
        self.assert_idle();
        require!(amount.0 > 0, "nothing to fund");
        let input = self.flow(flow_id).nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).input.clone();
        if input == NEAR {
            require!(amount.0 <= self.free_near(), "not enough free NEAR");
            self.credit(flow_id, node, amount.0);
            return PromiseOrValue::Value(());
        }
        let token: AccountId = input.parse().unwrap();
        PromiseOrValue::Promise(
            ext_ft_core::ext(token)
                .with_static_gas(GAS_VIEW)
                .ft_balance_of(env::current_account_id())
                .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(10)).on_fund_balance(flow_id, node, amount)),
        )
    }

    #[private]
    pub fn on_fund_balance(&mut self, flow_id: u32, node: u16, amount: U128, #[callback_unwrap] balance: U128) {
        self.assert_idle();
        let token = self.flow(flow_id).nodes[node as usize].input.clone();
        let free = balance.0.saturating_sub(self.allocated(&token));
        require!(amount.0 <= free, "not enough free balance");
        self.credit(flow_id, node, amount.0);
    }

    /// Stop a flow: everything waiting in it goes back to the free balance.
    pub fn drain(&mut self, flow_id: u32) {
        self.assert_owner();
        let f = self.flow_mut(flow_id);
        let rec = f;
        if let Err(e) = rec.ledger.drain(&rec.nodes) {
            env::panic_str(&format!("{e:?}"));
        }
    }

    /// Un-pause a node after failures.
    pub fn resume(&mut self, flow_id: u32, node: u16) {
        self.assert_owner();
        let f = self.flow_mut(flow_id);
        if let Err(e) = f.ledger.resume(node) {
            env::panic_str(&format!("{e:?}"));
        }
    }

    /// Clear a step whose callback never came back. What it held is treated as
    /// spent by the ledger; whatever is really still in the vault becomes free.
    pub fn unstick(&mut self) {
        self.assert_owner();
        let ctx = self.busy.clone().unwrap_or_else(|| env::panic_str("nothing running"));
        require!(env::block_timestamp() >= ctx.started_ns + STUCK_NS, "the step may still finish");
        self.busy = None;
        let f = self.flow_mut(ctx.flow);
        let _ = f.ledger.finish(&f.nodes, ctx.node, Err(0));
        f.ledger.pause(ctx.node).ok();
    }

    /// Withdraw free balance to the owner. "near" for native NEAR.
    pub fn withdraw(&mut self, token: String, amount: U128) -> Promise {
        self.assert_owner();
        self.assert_idle();
        if token == NEAR {
            require!(amount.0 <= self.free_near(), "not enough free NEAR");
            return Promise::new(self.owner.clone()).transfer(NearToken::from_yoctonear(amount.0));
        }
        let t: AccountId = token.parse().unwrap_or_else(|_| env::panic_str("bad token"));
        ext_ft_core::ext(t.clone())
            .with_static_gas(GAS_VIEW)
            .ft_balance_of(env::current_account_id())
            .then(Self::ext(env::current_account_id()).with_static_gas(Gas::from_tgas(20)).on_withdraw_balance(t, amount))
    }

    #[private]
    pub fn on_withdraw_balance(&mut self, token: AccountId, amount: U128, #[callback_unwrap] balance: U128) -> Promise {
        self.assert_idle();
        let free = balance.0.saturating_sub(self.allocated(token.as_str()));
        require!(amount.0 <= free, "not enough free balance");
        ext_ft_core::ext(token)
            .with_attached_deposit(NearToken::from_yoctonear(1))
            .with_static_gas(GAS_FT_TRANSFER)
            .ft_transfer(self.owner.clone(), amount, None)
    }

    // ---------- views ----------

    pub fn get_owner(&self) -> AccountId {
        self.owner.clone()
    }

    pub fn get_keeper(&self) -> Option<AccountId> {
        self.keeper.clone()
    }

    pub fn get_flows(&self) -> Vec<(u32, String)> {
        self.flows.iter().enumerate().filter_map(|(i, f)| f.as_ref().map(|f| (i as u32, f.name.clone()))).collect()
    }

    pub fn get_flow(&self, flow_id: u32) -> Option<FlowRec> {
        self.flows.get(flow_id as usize).cloned().flatten()
    }

    pub fn get_trusted(&self) -> Vec<Trust> {
        self.trusted.clone()
    }

    pub fn get_busy(&self) -> Option<StepCtx> {
        self.busy.clone()
    }

    /// How much of a token the vault's flows are holding (not free to withdraw).
    pub fn get_allocated(&self, token: String) -> U128 {
        U128(self.allocated(&token))
    }

    pub fn get_free_near(&self) -> U128 {
        U128(self.free_near())
    }
}

/// The vault accepts any token sent to it; it becomes free balance.
#[near]
impl FungibleTokenReceiver for Vault {
    fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> PromiseOrValue<U128> {
        let _ = (sender_id, amount, msg);
        PromiseOrValue::Value(U128(0))
    }
}

/// floor(a * b / d) in 256 bits; saturates instead of panicking (callbacks must not panic).
pub(crate) fn mul_div(a: u128, b: u128, d: u128) -> u128 {
    use math::wide::U256;
    if d == 0 {
        return u128::MAX;
    }
    let r = U256::from(a) * U256::from(b) / U256::from(d);
    if r > U256::from(u128::MAX) {
        u128::MAX
    } else {
        r.as_u128()
    }
}

impl Vault {
    pub(crate) fn assert_owner(&self) {
        require!(env::predecessor_account_id() == self.owner, "owner only");
    }

    fn assert_idle(&self) {
        require!(self.busy.is_none(), "a step is running");
    }

    fn flow(&self, id: u32) -> &FlowRec {
        self.flows.get(id as usize).and_then(Option::as_ref).unwrap_or_else(|| env::panic_str("no such flow"))
    }

    fn flow_mut(&mut self, id: u32) -> &mut FlowRec {
        self.flows.get_mut(id as usize).and_then(Option::as_mut).unwrap_or_else(|| env::panic_str("no such flow"))
    }

    /// Why node `n` isn't allowed to run, if it isn't: a contract it touches
    /// (input/output token, receiver, called contract, price source) isn't
    /// trusted, or it calls a method that isn't trusted on that contract.
    pub(crate) fn untrusted(&self, n: &Node<Recipe>) -> Option<String> {
        let me = env::current_account_id();
        let trust = |a: &AccountId| self.trusted.iter().find(|t| t.contract == *a && *a != me);
        let contract_ok = |a: &AccountId| trust(a).map_or(Some(format!("contract {a}")), |_| None);
        let method_ok = |a: &AccountId, m: &str| match trust(a) {
            None => Some(format!("contract {a}")),
            Some(_) if NEVER_CALL.contains(&m) => Some(format!("{a}.{m} moves funds")),
            Some(t) if !t.methods.iter().any(|x| x == m) => Some(format!("method {a}.{m}")),
            Some(_) => None,
        };
        if n.input != NEAR {
            let t: AccountId = n.input.parse().unwrap_or_else(|_| env::panic_str("bad input token"));
            if let Some(w) = contract_ok(&t) {
                return Some(w);
            }
        }
        for c in &n.action.calls {
            let why = match c {
                Call::Ft { receiver, .. } => contract_ok(receiver),
                Call::Fn { contract, method, .. } => method_ok(contract, method),
            };
            if why.is_some() {
                return why;
            }
        }
        if let Some(w) = n.action.out.as_ref().and_then(|o| contract_ok(o)) {
            return Some(w);
        }
        n.action.price.as_ref().and_then(|p| method_ok(&p.contract, &p.method))
    }

    fn credit(&mut self, flow_id: u32, node: u16, amount: u128) {
        let f = self.flow_mut(flow_id);
        if let Err(e) = f.ledger.fund(&f.nodes, node, amount) {
            env::panic_str(&format!("{e:?}"));
        }
    }

    /// What all flows hold of `token`.
    pub(crate) fn allocated(&self, token: &str) -> u128 {
        self.flows.iter().flatten().map(|f| f.ledger.holdings(&f.nodes).get(token).copied().unwrap_or(0)).sum()
    }

    /// NEAR not held by flows, not needed for storage, minus a small reserve.
    pub(crate) fn free_near(&self) -> u128 {
        let storage = env::storage_byte_cost().as_yoctonear() * env::storage_usage() as u128;
        env::account_balance()
            .as_yoctonear()
            .saturating_sub(storage)
            .saturating_sub(RESERVE.as_yoctonear())
            .saturating_sub(self.allocated(NEAR))
    }

    pub(crate) fn emit(&self, event: &str, data: &str) {
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"{event}","data":[{data}]}}"#));
    }
}
