//! The vault BRAIN: one shared, immutable contract holding every relay vault's
//! rules and state (keyed by the vault's account). A vault (relay/src/lib.rs)
//! forwards each request here with who sent it; the brain decides and answers
//! with the actions the vault must execute, as the vault. Callbacks (balances,
//! call results) come back through the vault, so the brain always knows which
//! vault is talking: the predecessor. A vault can only ever touch its own record.
//!
//! WHY: the full vault is ~600 KB, 59 NEAR to publish per version. The body is
//! 9 KB (~1 NEAR); the brain is deployed normally (storage staked, refundable).
//!
//! # Porting plan (everything not in this prototype)
//!
//! Rule of thumb: pure checks and state move into the brain unchanged; anything
//! that must be done AS the vault (move money, sign with the vault's MPC path,
//! register keys) becomes a CALL action the brain hands back.
//!
//! - limits: pure state; `charge` runs in the brain when a step starts. Ports as is.
//! - hidden: fingerprints live in the brain; `step_hidden` is a request carrying
//!   the revealed node (as public as today: it's in the vault's tx args). As is.
//! - oracle: price reads are views, and views don't need to come from the vault:
//!   the brain can read Pyth itself (its own promise + private callback) and
//!   return its answer as that promise's result; NEAR hands the final value to
//!   the vault's `cb`. Same for price triggers. One relay round saved per read.
//!   (The step's balance views could move the same way: measured in the brain,
//!   only the recipe's calls go through the vault. Not done here, for clarity.)
//! - evm / sol / soltemplates: evmtx/solguard run in the brain on the request;
//!   the answer is a CALL `signer.sign{payload, path}` from the vault (so the
//!   derived address is still per vault: MPC derives from predecessor + path),
//!   then a brain callback assembles the signed tx and returns it to the keeper.
//!   The vault's derived keys are views (`derived_public_key` takes the
//!   predecessor as an argument), so the brain can learn them directly.
//! - intents: `add_public_key` on intents.near and `sign` are CALLs from the
//!   vault; intent building/checking (intentsig) runs in the brain. Withdrawals
//!   to the vault arrive as `mt_on_transfer`/`ft_on_transfer` hooks: see below.
//! - stealth (being built in parallel): same rule; anything it needs from the
//!   vault's own account (a key, a signature path) becomes a CALL action.
//!
//! What can't port as is, and must be settled BEFORE the relay is published
//! (it is immutable, so its exports are forever):
//! - Receiver hooks. The relay answers `ft_on_transfer` itself ("keep it all").
//!   `nft_on_transfer` and `mt_on_transfer` (intents withdrawals, multi-tokens)
//!   don't exist yet: add them (or a generic hook forwarder that tags the
//!   method and asks the brain, whose answer becomes the hook's return value).
//! - The vault's synchronous state: balance and storage are a snapshot in the
//!   header at call time; a decision can't re-read them mid-call. Today's
//!   `free_near` works from the snapshot; a race with another receipt just
//!   makes the vault's transfer fail, never the brain's books.
//! - Atomicity: the brain commits, then the vault acts in a later receipt. A
//!   vault that can't execute (out of gas or NEAR) leaves the brain believing a
//!   step is in flight: the existing `unstick` covers it. Step ids (below)
//!   stop a late callback from an old step landing in a new one.
//! - Gas: each relay round costs a receipt (~a few Tgas). Long chains (oracle +
//!   price + several calls) must fit in 300 Tgas: see `need_tgas`.
//!
//! Storage: the brain pays for every vault's record and charges it back: when a
//! record grows past what its vault paid, the answer includes a `pay` call
//! from the vault for the difference (plus a small buffer). A vault that can't
//! afford it is refused the change.

mod step;
pub mod wire;

use flow::{Action, Ledger, Node, Token};
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, PanicOnDefault};

pub use step::StepCtx;
use wire::{Answer, Ctx};

pub const MAX_FLOWS: usize = 16;
pub const MAX_CALLS: usize = 4;
pub const MAX_RECIPE_TGAS: u64 = 200;
/// NEAR each vault always keeps for its own storage and fees.
pub const RESERVE: u128 = 50_000_000_000_000_000_000_000;
pub const STUCK_NS: u64 = 10 * 60 * 1_000_000_000;
pub const PRICE_SCALE: u128 = 1_000_000_000_000_000_000_000_000;
pub const NEAR: &str = "near";
/// Every storage record costs its key, its value and 40 bytes of bookkeeping.
const RECORD_OVERHEAD: u64 = 40;
/// Paid on top of what's owed, so not every small change needs a payment.
const PAY_BUFFER_BYTES: u128 = 500;

pub(crate) const GAS_VIEW: Gas = Gas::from_tgas(5);
pub(crate) const GAS_BRAIN_CB: Gas = Gas::from_tgas(15);
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_PAY: Gas = Gas::from_tgas(5);

#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Deposit {
    None,
    OneYocto,
    Input,
    Fixed { yocto: U128 },
}

/// One contract call in a recipe (same as the vault's; see vault/src/lib.rs).
#[near(serializers = [borsh, json])]
#[serde(tag = "call", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Ft { receiver: AccountId, msg: String, tgas: u64 },
    Fn { contract: AccountId, method: String, args: String, deposit: Deposit, tgas: u64 },
}

impl Call {
    fn spends_input(&self) -> bool {
        matches!(self, Call::Ft { .. } | Call::Fn { deposit: Deposit::Input, .. })
    }
    pub(crate) fn tgas(&self) -> u64 {
        match self {
            Call::Ft { tgas, .. } | Call::Fn { tgas, .. } => *tgas,
        }
    }
}

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
    pub label: String,
    pub calls: Vec<Call>,
    pub out: Option<AccountId>,
    pub min_out_per_in: Option<U128>,
    pub price: Option<PriceSource>,
    #[serde(default)]
    pub scale_per_in: Option<U128>,
}

impl Action for Recipe {
    fn output(&self, _input: &Token) -> Option<Token> {
        self.out.as_ref().map(|a| a.to_string())
    }
    fn accepts(&self, input: &Token) -> bool {
        let spenders: Vec<&Call> = self.calls.iter().filter(|c| c.spends_input()).collect();
        let right_kind = match spenders.as_slice() {
            [] => input == flow::NO_INPUT && self.out.is_some(),
            [Call::Ft { .. }] => input != NEAR && input != flow::NO_INPUT,
            [Call::Fn { .. }] => input == NEAR,
            _ => false,
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

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Trust {
    pub contract: AccountId,
    #[serde(default)]
    pub methods: Vec<String>,
}

/// Methods that move funds out (same list as the vault). Never callable by a recipe.
pub const NEVER_CALL: &[&str] = &[
    "ft_transfer", "ft_transfer_call", "nft_transfer", "nft_transfer_call", "mt_transfer", "mt_transfer_call",
    "mt_batch_transfer", "mt_batch_transfer_call", "near_withdraw", "storage_withdraw", "storage_unregister",
    "nft_approve", "ft_approve", "mt_approve", "ft_withdraw", "mt_withdraw", "nft_withdraw", "native_withdraw",
    "add_public_key", "remove_public_key", "execute_intents", "set_auth_by_predecessor_id", "invalidate_nonces",
    "sign", "sign_v2", "request_app_private_key", "simple_withdraw",
];
// New with the split: the brain itself can never be trusted (see `trust` and
// `untrusted`): a recipe calling it from the vault would talk to the brain AS
// the vault, outside a request.

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct FlowRec {
    pub name: String,
    pub nodes: Vec<Node<Recipe>>,
    pub inputs: Vec<Token>,
    pub ledger: Ledger,
}

/// One vault's record in the brain.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct VaultState {
    pub owner: AccountId,
    pub keeper: Option<AccountId>,
    pub flows: Vec<Option<FlowRec>>,
    pub busy: Option<StepCtx>,
    pub trusted: Vec<Trust>,
    /// NEAR this vault paid the brain for its record's storage.
    pub paid: U128,
    /// Next step id: a callback must carry the id of the step in flight.
    pub next_step: u64,
}

/// Requests a vault forwards (its caller's JSON args), e.g. {"fund": {"flow_id": 0, "node": 0, "amount": "10"}}.
#[near(serializers = [json])]
#[serde(rename_all = "snake_case")]
pub enum Req {
    Init { owner: AccountId, keeper: Option<AccountId> },
    SetKeeper { keeper: Option<AccountId> },
    Trust { add: Vec<Trust>, remove: Vec<AccountId> },
    SaveFlow { name: String, nodes: Vec<Node<Recipe>> },
    DeleteFlow { flow_id: u32 },
    Fund { flow_id: u32, node: u16, amount: U128 },
    Drain { flow_id: u32 },
    Resume { flow_id: u32, node: u16 },
    Unstick {},
    Withdraw { token: String, amount: U128 },
    Step { flow_id: u32, node: u16 },
}

#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) struct FundArgs {
    flow_id: u32,
    node: u16,
    amount: u128,
}

#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) struct WithdrawArgs {
    token: AccountId,
    amount: u128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Brain {
    /// Only sub-accounts of this factory are vaults, and only it can make them.
    pub factory: AccountId,
}

/// A vault talking to the brain: who it is, what it reported, its record.
pub(crate) struct Talk {
    pub vault: AccountId,
    pub ctx: Ctx,
    pub vs: VaultState,
}

#[near]
impl Brain {
    #[init]
    pub fn new(factory: AccountId) -> Self {
        Self { factory }
    }

    /// A request forwarded by a vault (relay `x` or `new`).
    pub fn handle(&self) {
        let input = env::input().unwrap_or_default();
        let (ctx, body) = wire::parse(&input);
        let sender = ctx.sender.clone().unwrap_or_else(|| env::panic_str("a request names its sender"));
        let vault = env::predecessor_account_id();
        let req: Req = near_sdk::serde_json::from_slice(body).unwrap_or_else(|e| env::panic_str(&format!("bad request: {e}")));
        if let Req::Init { owner, keeper } = req {
            return self.init(vault, ctx, sender, owner, keeper);
        }
        let vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        let mut t = Talk { vault, ctx, vs };
        let a = t.request(sender, req);
        t.save(a).send();
    }

    /// A vault paying for its record (asked for in an answer).
    #[payable]
    pub fn pay(&mut self) {
        let vault = env::predecessor_account_id();
        let mut vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        vs.paid = U128(vs.paid.0 + env::attached_deposit().as_yoctonear());
        store(&vault, &vs);
    }

    // ---------- callbacks: always from the vault, on results of its own actions ----------

    pub fn on_fund(&self) {
        let (mut t, a): (Talk, FundArgs) = callback();
        t.idle();
        let token = t.flow(a.flow_id).inputs[a.node as usize].clone();
        let balance = amount_result(0).unwrap_or_else(|| env::panic_str("balance unknown"));
        let free = balance.saturating_sub(t.allocated(&token));
        require!(a.amount <= free, "not enough free balance");
        t.credit(a.flow_id, a.node, a.amount);
        t.save(Answer::default()).send();
    }

    pub fn on_withdraw(&self) {
        let (t, a): (Talk, WithdrawArgs) = callback();
        t.idle();
        let balance = amount_result(0).unwrap_or_else(|| env::panic_str("balance unknown"));
        let free = balance.saturating_sub(t.allocated(a.token.as_str()));
        require!(a.amount <= free, "not enough free balance");
        let args = format!(r#"{{"receiver_id":"{}","amount":"{}"}}"#, t.vs.owner, a.amount);
        let ans = Answer::default().call(&a.token, "ft_transfer", args.as_bytes(), 1, GAS_FT_TRANSFER);
        t.save(ans).send();
    }

    // ---------- views ----------

    pub fn get_factory(&self) -> AccountId {
        self.factory.clone()
    }

    pub fn get_vault(&self, vault: AccountId) -> Option<VaultState> {
        load(&vault)
    }

    pub fn get_flow(&self, vault: AccountId, flow_id: u32) -> Option<FlowRec> {
        load(&vault).and_then(|v| v.flows.get(flow_id as usize).cloned().flatten())
    }

    pub fn get_allocated(&self, vault: AccountId, token: String) -> U128 {
        let vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        U128(allocated(&vs, &token))
    }

    /// What this vault's record costs and what it has paid.
    pub fn get_storage(&self, vault: AccountId) -> (U128, U128) {
        let vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        (U128(record_cost(&vault, &vs)), vs.paid)
    }
}

impl Brain {
    /// The factory made this vault (a direct sub-account of the factory, and the
    /// factory itself called the vault's `new`): give it an empty record.
    fn init(&self, vault: AccountId, ctx: Ctx, sender: AccountId, owner: AccountId, keeper: Option<AccountId>) {
        require!(sender == self.factory, "vaults are made by the factory");
        let name = vault.as_str().strip_suffix(&format!(".{}", self.factory)).unwrap_or("");
        require!(!name.is_empty() && !name.contains('.'), "not one of the factory's vaults");
        require!(load(&vault).is_none(), "already a vault");
        let vs = VaultState { owner, keeper, flows: Vec::new(), busy: None, trusted: Vec::new(), paid: U128(0), next_step: 0 };
        Talk { vault, ctx, vs }.save(Answer::default()).send();
    }
}

impl Talk {
    fn request(&mut self, sender: AccountId, req: Req) -> Answer {
        let owner = sender == self.vs.owner;
        let owner_only = || require!(owner, "owner only");
        match req {
            Req::Init { .. } => unreachable!(),
            Req::Step { flow_id, node } => return self.begin_step(&sender, flow_id, node),
            Req::SetKeeper { keeper } => {
                owner_only();
                self.vs.keeper = keeper;
            }
            Req::Trust { add, remove } => {
                owner_only();
                self.trust(add, remove);
            }
            Req::SaveFlow { name, nodes } => {
                owner_only();
                let id = self.save_flow(name, nodes);
                return Answer::default().ret(&id.to_string());
            }
            Req::DeleteFlow { flow_id } => {
                owner_only();
                self.idle();
                let f = self.flow(flow_id);
                require!(f.ledger.holdings_inputs(&f.inputs).is_empty(), "drain the flow first");
                self.vs.flows[flow_id as usize] = None;
            }
            Req::Fund { flow_id, node, amount } => {
                owner_only();
                self.idle();
                require!(amount.0 > 0, "nothing to fund");
                let input = self.flow(flow_id).inputs.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
                if input == NEAR {
                    require!(amount.0 <= self.free_near(), "not enough free NEAR");
                    self.credit(flow_id, node, amount.0);
                } else {
                    let token: AccountId = input.parse().unwrap();
                    let args = borsh::to_vec(&FundArgs { flow_id, node, amount: amount.0 }).unwrap();
                    return self.balance_of(&token).then("on_fund", args, GAS_BRAIN_CB);
                }
            }
            Req::Drain { flow_id } => {
                owner_only();
                let f = self.flow_mut(flow_id);
                f.ledger.drain_inputs(&f.inputs).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
            }
            Req::Resume { flow_id, node } => {
                owner_only();
                self.flow_mut(flow_id).ledger.resume(node).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
            }
            Req::Unstick {} => {
                owner_only();
                self.unstick();
            }
            Req::Withdraw { token, amount } => {
                owner_only();
                self.idle();
                if token == NEAR {
                    require!(amount.0 <= self.free_near(), "not enough free NEAR");
                    return Answer::default().transfer(&self.vs.owner.clone(), amount.0);
                }
                let t: AccountId = token.parse().unwrap_or_else(|_| env::panic_str("bad token"));
                let args = borsh::to_vec(&WithdrawArgs { token: t.clone(), amount: amount.0 }).unwrap();
                return self.balance_of(&t).then("on_withdraw", args, GAS_BRAIN_CB);
            }
        }
        Answer::default()
    }

    fn trust(&mut self, add: Vec<Trust>, remove: Vec<AccountId>) {
        self.vs.trusted.retain(|t| !remove.contains(&t.contract));
        for t in add {
            require!(t.contract != self.vault, "a vault never trusts itself");
            require!(t.contract != env::current_account_id(), "a vault never trusts the brain");
            for m in &t.methods {
                require!(!NEVER_CALL.contains(&m.as_str()), format!("{m} moves funds: recipes can never call it"));
                require!(!m.is_empty() && m.len() <= 64, "bad method");
            }
            require!(t.methods.len() <= 16, "too many methods");
            self.vs.trusted.retain(|x| x.contract != t.contract);
            self.vs.trusted.push(t);
        }
        require!(self.vs.trusted.len() <= 64, "too many trusted contracts");
    }

    fn save_flow(&mut self, name: String, nodes: Vec<Node<Recipe>>) -> u32 {
        require!(name.len() <= 60, "name too long");
        if let Err(e) = flow::validate(&nodes) {
            env::panic_str(&format!("bad flow: {e:?}"));
        }
        for n in &nodes {
            if let Some(why) = self.untrusted(n) {
                env::panic_str(&format!("untrusted: {why}"));
            }
        }
        let rec = FlowRec { name, ledger: Ledger::new(nodes.len()), inputs: flow::inputs(&nodes), nodes };
        if let Some(i) = self.vs.flows.iter().position(Option::is_none) {
            self.vs.flows[i] = Some(rec);
            return i as u32;
        }
        require!(self.vs.flows.len() < MAX_FLOWS, "too many flows");
        self.vs.flows.push(Some(rec));
        (self.vs.flows.len() - 1) as u32
    }

    pub(crate) fn balance_of(&self, token: &AccountId) -> Answer {
        Answer::default().call(token, "ft_balance_of", format!(r#"{{"account_id":"{}"}}"#, self.vault).as_bytes(), 0, GAS_VIEW)
    }

    pub(crate) fn idle(&self) {
        require!(self.vs.busy.is_none(), "a step is running");
    }

    pub(crate) fn flow(&self, id: u32) -> &FlowRec {
        self.vs.flows.get(id as usize).and_then(Option::as_ref).unwrap_or_else(|| env::panic_str("no such flow"))
    }

    pub(crate) fn flow_mut(&mut self, id: u32) -> &mut FlowRec {
        self.vs.flows.get_mut(id as usize).and_then(Option::as_mut).unwrap_or_else(|| env::panic_str("no such flow"))
    }

    /// Why node `n` may not run, if it may not (same rules as the vault).
    pub(crate) fn untrusted(&self, n: &Node<Recipe>) -> Option<String> {
        let me = &self.vault;
        let brain = env::current_account_id();
        let trust = |a: &AccountId| self.vs.trusted.iter().find(|t| t.contract == *a && a != me && *a != brain);
        let contract_ok = |a: &AccountId| trust(a).map_or(Some(format!("contract {a}")), |_| None);
        let method_ok = |a: &AccountId, m: &str| match trust(a) {
            None => Some(format!("contract {a}")),
            Some(_) if NEVER_CALL.contains(&m) => Some(format!("{a}.{m} moves funds")),
            Some(t) if !t.methods.iter().any(|x| x == m) => Some(format!("method {a}.{m}")),
            Some(_) => None,
        };
        if n.input != NEAR && n.input != flow::NO_INPUT {
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

    pub(crate) fn credit(&mut self, flow_id: u32, node: u16, amount: u128) {
        let f = self.flow_mut(flow_id);
        f.ledger.fund(&f.nodes, node, amount).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
    }

    pub(crate) fn allocated(&self, token: &str) -> u128 {
        allocated(&self.vs, token)
    }

    /// The vault's NEAR not held by flows, not needed for its own storage, minus
    /// the reserve. From the balance the vault reported in this call.
    pub(crate) fn free_near(&self) -> u128 {
        let storage = env::storage_byte_cost().as_yoctonear() * self.ctx.storage as u128;
        self.ctx.balance.saturating_sub(storage).saturating_sub(RESERVE).saturating_sub(self.allocated(NEAR))
    }

    /// Store the record; if it now costs more than the vault paid, the answer
    /// also makes the vault pay the difference (last action, so the brain's own
    /// callbacks still find their results at the indices they expect).
    pub(crate) fn save(self, ans: Answer) -> Answer {
        let owed = record_cost(&self.vault, &self.vs).saturating_sub(self.vs.paid.0);
        store(&self.vault, &self.vs);
        if owed == 0 {
            return ans;
        }
        let pay = owed + PAY_BUFFER_BYTES * env::storage_byte_cost().as_yoctonear();
        require!(pay <= self.free_near(), "the vault can't pay for its storage in the brain: add NEAR");
        ans.call(&env::current_account_id(), "pay", b"", pay, GAS_PAY)
    }

    pub(crate) fn emit(&self, event: &str, data: &str) {
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"{event}","data":[{{"vault":"{}",{data}}}]}}"#, self.vault));
    }
}

/// A brain callback: from a vault (predecessor), carrying no sender (only the
/// vault's own `cb` makes those), with the brain's own borsh args.
pub(crate) fn callback<A: BorshDeserialize>() -> (Talk, A) {
    let input = env::input().unwrap_or_default();
    let (ctx, body) = wire::parse(&input);
    require!(ctx.sender.is_none(), "callbacks come from the vault itself");
    let vault = env::predecessor_account_id();
    let vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
    let a = A::try_from_slice(body).unwrap_or_else(|_| env::panic_str("bad callback"));
    (Talk { vault, ctx, vs }, a)
}

/// A promise result (at most 1 KB: we only read amounts), or None if it failed.
pub(crate) fn result(i: u64) -> Option<Vec<u8>> {
    if i >= env::promise_results_count() {
        return None;
    }
    env::promise_result_checked(i, 1024).ok()
}

pub(crate) fn amount_result(i: u64) -> Option<u128> {
    result(i).and_then(|v| near_sdk::serde_json::from_slice::<U128>(&v).ok()).map(|a| a.0)
}

fn key(vault: &AccountId) -> Vec<u8> {
    [b"v:".as_slice(), vault.as_bytes()].concat()
}

fn load(vault: &AccountId) -> Option<VaultState> {
    env::storage_read(&key(vault)).map(|b| VaultState::try_from_slice(&b).unwrap_or_else(|_| env::panic_str("bad record")))
}

fn store(vault: &AccountId, vs: &VaultState) {
    env::storage_write(&key(vault), &borsh::to_vec(vs).unwrap());
}

fn record_cost(vault: &AccountId, vs: &VaultState) -> u128 {
    let bytes = RECORD_OVERHEAD + key(vault).len() as u64 + borsh::to_vec(vs).unwrap().len() as u64;
    bytes as u128 * env::storage_byte_cost().as_yoctonear()
}

fn allocated(vs: &VaultState, token: &str) -> u128 {
    vs.flows.iter().flatten().map(|f| f.ledger.holdings_inputs(&f.inputs).get(token).copied().unwrap_or(0)).sum()
}

/// floor(a * b / d) in 256 bits; saturates.
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
