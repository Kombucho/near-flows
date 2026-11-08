//! The vault BRAIN: one shared, immutable contract holding every relay vault's
//! rules and state (keyed by the vault's account). A vault (relay/src/lib.rs)
//! forwards each request here with who sent it; the brain decides and answers
//! with the actions the vault must execute, as the vault. The brain always
//! knows which vault is talking: the predecessor. A vault only ever touches
//! its own record.
//!
//! WHY: the old full vault (one ~640 KB contract per user, removed 2026-11) cost
//! ~64 NEAR to publish per version. The body is ~10 KB (~1 NEAR); the brain is
//! deployed normally (storage staked, refundable).
//!
//! Three ways in, each with its own proof of who's talking:
//! - `handle`: a request the vault forwards (relay `x`, a named door, `new`).
//!   The header names the vault's real caller; the brain checks owner/keeper.
//! - `on_vault`: the vault's callback on actions the brain asked it to run
//!   (their results are the vault's to report). Only the vault's own `cb` makes
//!   these, after the brain named this method in an answer: no sender in the
//!   header, predecessor = a vault with a record.
//! - `on_read`: the brain's OWN reads (balances, prices, the MPC key, the
//!   Intents salt): views need no vault, so the brain issues them itself and
//!   decides in this private callback; its answer becomes the value the vault's
//!   `cb` receives. One relay round less per read. #[private]: only the brain.
//!
//! # What the brain does
//!
//! Everything the old full vault did, with the same request names and arguments
//! its methods had (apps and keepers call the relay's `x` door with
//! `{"<method>": <args>}`; stealth relayers call `exec`):
//! - core: trust, save/delete/fund/drain/resume/unstick, step, withdraw,
//!   withdraw_to, set_keeper, act.
//! - limits, hidden (fingerprints pinned with the keeper's, remote nodes
//!   included), stealth (same signed text as the keeper's), intents.
//! - oracle: market floors on recipes, EVM leaves, Solana pairs and Intents
//!   pairs; Pyth / priceboard reads issued by the brain itself.
//! - evm: templates and rule trees, built and checked here, signed by the
//!   vault's `sign` (the address stays per vault).
//! - sol + soltemplates: Jupiter checks and instruction templates (owner
//!   co-sign), signed by the vault's `sign`.
//! - remote: flow nodes on other chains, keeper-reported outputs capped and floored.
//!
//! What differs from the old full vault:
//! - The vault's synchronous state: balance and storage are a snapshot in the
//!   header. A race with another receipt only makes the vault's transfer fail,
//!   never the brain's books.
//! - Atomicity: the brain commits, the vault acts in a later receipt. A vault
//!   that can't execute (out of gas or NEAR) leaves a step "in flight": the
//!   existing `unstick` covers it; step ids stop a late callback landing in a
//!   new step.
//! - Hooks: NEAR has no fallback method, so a callback name the relay doesn't
//!   export can never reach the brain (see relay/src/lib.rs, "named doors").
//!
//! Storage: the brain pays for every vault's record and charges it back: when a
//! record grows past what its vault paid, the answer includes a `pay` call
//! from the vault for the difference (plus a small buffer). A vault that can't
//! afford it is refused the change.

mod close;
pub mod fees;
pub mod evm;
mod hidden;
pub mod intents;
mod limits;
pub mod oracle;
pub mod remote;
pub mod sol;
mod stealth;
mod step;
pub mod wire;

use flow::{Action, Ledger, Node, Token};
use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::json_types::{U128, U64};
use near_sdk::{env, near, require, AccountId, Gas, NearToken, PanicOnDefault, Promise, PublicKey};

pub use hidden::fingerprint;
pub use intents::{IntentsConfig, IntentsPair};
pub use limits::{Limit, PendingLimit};
pub use oracle::{Market, OracleConfig};
pub use remote::Remote;
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

// Gas. Mainnet caps a transaction at 300 Tgas prepaid, and a recipe's own calls
// may ask for most of it (the library's Borrow on Rhea: 60 + 120), so every
// reserve below is a measured burn (sandbox, GAS_RECEIPTS=1 in gas_mainnet.rs
// and protocols.rs) times a stated margin, not a round guess.

/// What the relay gives `handle` (relay GAS_HANDLE): the next round has the rest.
/// Enough to decide (handle burns 2.8-5.2 Tgas) AND read two balances in place
/// (`read_tgas`: 2 x 8 + 8 + 3 = 27), so a plain step needs no extra round. A
/// heavier first read (price trigger, oracle feeds) moves to the vault's next
/// round instead (`read`), paid from what the caller attached.
pub(crate) const HANDLE_TGAS: u64 = 40;
/// A view: ft_balance_of on the real wNEAR, Rhea's get_return and the mock Pyth
/// burn 1.2-1.3 Tgas; 5 leaves room for a bigger real contract (Pyth's) to load.
pub(crate) const GAS_VIEW: Gas = Gas::from_tgas(5);
/// What creating a function-call receipt costs its creator beyond the gas it
/// attaches (the new receipt's prepaid execution fee, ~2.4 Tgas), rounded up.
pub(crate) const RECEIPT_TGAS: u64 = 3;
/// A brain callback's own work around the promises it makes (load and store the
/// record, decide, encode): brain receipts burn 2.4-5.3 Tgas; 6 holds the
/// common ones, and those that build transactions get their own constants.
pub(crate) const OWN_TGAS: u64 = 6;
/// The brain's own read callback when it only answers (on_read burns 2.4-3.7).
pub(crate) const GAS_READ: Gas = Gas::from_tgas(8);
/// The vault's `cb` per round (relay GAS_CB: cb burns 0.9-2.6 Tgas).
pub(crate) const RELAY_TGAS: u64 = 5;
/// What a callback still spends after making its promises: storing the record
/// and returning (part of the 2.4-5.3 burnt above).
pub(crate) const FINISH_TGAS: u64 = 3;
/// The relay's `x` itself: burns 1.7-1.8 Tgas and prepays two receipts (~5).
pub(crate) const X_TGAS: u64 = 5;
/// Mainnet's cap on a transaction's prepaid gas.
pub const MAINNET_TGAS: u64 = 300;
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(10);
const GAS_PAY: Gas = Gas::from_tgas(5);
const GAS_ACT: Gas = Gas::from_tgas(100);

#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Deposit {
    None,
    OneYocto,
    Input,
    Fixed { yocto: U128 },
}

/// One contract call in a recipe.
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
    #[serde(default)]
    pub market: Option<Market>,
    /// A node on another chain (remote.rs): `calls` empty, `out` None.
    #[serde(default)]
    pub remote: Option<Remote>,
}

impl Action for Recipe {
    fn output(&self, _input: &Token) -> Option<Token> {
        if let Some(r) = &self.remote {
            return r.out.clone();
        }
        self.out.as_ref().map(|a| a.to_string())
    }
    fn accepts(&self, input: &Token) -> bool {
        if let Some(r) = &self.remote {
            return self.calls.is_empty() && self.out.is_none() && self.label.len() <= 60 && r.accepts(input);
        }
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

/// Methods that move funds out (same list as the vault). Never callable by a
/// recipe or `act`. The brain itself can't be trusted at all (see `trust`).
pub const NEVER_CALL: &[&str] = &[
    "ft_transfer", "ft_transfer_call", "nft_transfer", "nft_transfer_call", "mt_transfer", "mt_transfer_call",
    "mt_batch_transfer", "mt_batch_transfer_call", "near_withdraw", "storage_withdraw", "storage_unregister",
    "nft_approve", "ft_approve", "mt_approve", "ft_withdraw", "mt_withdraw", "nft_withdraw", "native_withdraw",
    "add_public_key", "remove_public_key", "execute_intents", "set_auth_by_predecessor_id", "invalidate_nonces",
    "sign", "sign_v2", "request_app_private_key", "simple_withdraw",
];

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct FlowRec {
    pub name: String,
    /// The nodes, readable by anyone. Empty while the flow is hidden.
    pub nodes: Vec<Node<Recipe>>,
    pub inputs: Vec<Token>,
    /// A hidden flow's fingerprints, one per node. Empty when open.
    pub hidden: Vec<String>,
    pub ledger: Ledger,
}

impl FlowRec {
    pub fn is_hidden(&self) -> bool {
        !self.hidden.is_empty()
    }
}

/// One vault's record in the brain.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct VaultState {
    /// The owner's account; a stealth vault is its own owner (see stealth.rs).
    pub owner: AccountId,
    pub keeper: Option<AccountId>,
    pub flows: Vec<Option<FlowRec>>,
    pub busy: Option<StepCtx>,
    pub trusted: Vec<Trust>,
    pub limits: Vec<Limit>,
    pub pending: Vec<PendingLimit>,
    pub intents: Option<IntentsConfig>,
    pub oracle: Option<OracleConfig>,
    pub evm: Option<evm::EvmConfig>,
    pub sol: Option<sol::SolConfig>,
    /// Remote steps waiting for their report (remote.rs).
    pub remote: Vec<remote::Pending>,
    #[serde(skip)]
    pub stealth: Option<stealth::Stealth>,
    /// NEAR this vault paid the brain for its record's storage.
    pub paid: U128,
    /// Next step id: a callback must carry the id of the step in flight.
    pub next_step: u64,
    /// Auto-close on: anyone may `close` the empty vault, its NEAR going here (close.rs).
    pub auto_close: Option<AccountId>,
    /// The vault ever set up another chain (EVM, Solana, Intents, a remote node):
    /// its balances there can't be seen from here, so only its owner may close it.
    pub crosschain: bool,
    /// NEAR paid to the keeper per run it did successfully (set by the owner, fees.rs).
    pub keeper_fee: U128,
    /// Protocol fees in tokens, set aside out of the free balance until settled (fees.rs).
    pub owed_fees: Vec<(String, U128)>,
}

/// Requests a vault forwards: `{"<method>": {<args>}}`, the vault's own method
/// names and arguments.
#[near(serializers = [json])]
#[serde(rename_all = "snake_case")]
pub enum Req {
    // Made by the factory.
    Init { owner: AccountId, keeper: Option<AccountId> },
    InitStealth { owner_key: PublicKey, keeper: Option<AccountId> },
    // Owner.
    /// The keeper and its price per successful run (0 for your own keeper).
    SetKeeper { keeper: Option<AccountId>, #[serde(default)] keeper_fee: U128 },
    Trust { add: Vec<Trust>, remove: Vec<AccountId> },
    SaveFlow { name: String, nodes: Vec<Node<Recipe>> },
    DeleteFlow { flow_id: u32 },
    Fund { flow_id: u32, node: u16, amount: U128 },
    Drain { flow_id: u32 },
    Resume { flow_id: u32, node: u16 },
    Unstick {},
    Withdraw { token: String, amount: U128 },
    Act { target: AccountId, method: String, json_args: String, one_yocto: bool },
    SetLimit { token: String, max: Option<U128>, window_ns: u64 },
    SaveHiddenFlow { name: String, inputs: Vec<Token>, fingerprints: Vec<String> },
    HideFlow { flow_id: u32, fingerprints: Vec<String> },
    RevealFlow { flow_id: u32, nodes: Vec<Node<Recipe>>, salts: Vec<String> },
    SetOwnerKey { owner_key: PublicKey },
    /// Auto-close: `to` is where the NEAR goes (a named vault: its owner; a stealth vault: required).
    SetAutoClose { on: bool, #[serde(default)] to: Option<AccountId> },
    WithdrawTo { token: String, amount: U128, to: AccountId, via_intents: bool },
    IntentsSetup { verifier: AccountId, signer: AccountId },
    IntentsAllow { pair: IntentsPair },
    OracleSetup { config: OracleConfig },
    EvmSetup { signer: AccountId, chain_id: u64, max_fee: U128, max_tip: U128, gas_cap: u64 },
    EvmTemplate { template: evm::EvmTemplate },
    EvmTemplateRemove { name: String },
    SolSetup { signer: AccountId, max_cu_price: u64, max_cu_limit: u32 },
    SolAllow { pair: sol::SolPair },
    SolOwnerWallet { wallet: Option<String> },
    SolTemplate { template: sol::SolTemplate },
    SolTemplateRemove { name: String },
    // Owner or keeper.
    Step { flow_id: u32, node: u16 },
    StepHidden { flow_id: u32, node: u16, reveal: Node<Recipe>, salt: String },
    IntentsSignSwap { token_in: AccountId, token_out: AccountId, amount_in: U128, min_out: U128, valid_secs: u64 },
    IntentsWithdraw { token: AccountId, amount: U128 },
    EvmCall { name: String, values: Vec<U128>, nonce: u64, max_fee: U128, max_tip: U128, gas: u64 },
    SolSignSwap { message: String },
    SolSignTx { message: String },
    StepRemote { flow_id: u32, node: u16, args: remote::RemoteArgs, #[serde(default)] reveal: Option<Node<Recipe>>, #[serde(default)] salt: Option<String> },
    RemoteReport { flow_id: u32, node: u16, out: Option<U128> },
    // Anyone.
    ApplyLimit { token: String },
    /// Send a token's owed protocol fees to the treasury (fees.rs).
    SettleFees { token: AccountId },
    /// Delete the empty vault (close.rs): anyone with auto-close on, else the owner.
    Close { #[serde(default)] confirm_remote: bool, #[serde(default)] to: Option<AccountId> },
    /// A stealth owner's signed command (see stealth.rs).
    Exec { method: String, args: String, nonce: U64, expires_ns: U64, tgas: u64, signature: String },
    // Receiver hooks (relay named doors): answered, never acted on.
    NftOnTransfer { sender_id: AccountId, previous_owner_id: AccountId, token_id: String, msg: String },
    MtOnTransfer { sender_id: AccountId, previous_owner_ids: Vec<AccountId>, token_ids: Vec<String>, amounts: Vec<U128>, msg: String },
}

/// Tgas a read takes from the receipt that makes it: each view's gas and receipt
/// fee, the callback's gas and its receipt fee.
pub(crate) fn read_tgas(views: usize, callback_tgas: u64) -> u64 {
    views as u64 * (GAS_VIEW.as_tgas() + RECEIPT_TGAS) + callback_tgas + RECEIPT_TGAS
}

/// A brain callback routed through the vault: it needs a result only the vault has.
#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) enum Next {
    /// A recipe call of the step `id` returned (`tgas`: what this callback was given).
    Call { id: u64, i: u8, tgas: u64 },
    /// The MPC signer answered the vault's `sign` for an intent.
    IntentsSigned { message: String, nonce: [u8; 32] },
    /// A stealth command whose nonce is already burned: run it as the owner.
    Exec { method: String, args: String, tgas: u64 },
    EvmSigned(evm::TxJson),
    SolSigned(String),
    RemoteEvmSigned { flow_id: u32, node: u16, tx: evm::TxJson },
    RemoteSolSigned { flow_id: u32, node: u16, hash: String },
    /// A read that didn't fit where it was asked for: the vault's next round makes it.
    Read { views: Vec<(AccountId, String, String)>, read: Read, gas: u64 },
    /// The owed fees' ft_transfer to the treasury returned.
    FeeSettled { token: String, amount: u128 },
}

/// The brain's own reads, and what to do with their results.
#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) enum Read {
    Measured { id: u64, before: bool },
    Price { flow_id: u32, node: u16, by_owner: bool, per: u128, revealed: Option<Node<Recipe>> },
    Fund { flow_id: u32, node: u16, amount: u128 },
    Withdraw { token: AccountId, amount: u128, to: AccountId, via_intents: bool },
    IntentsKey,
    IntentsSalt(intents::Pending),
    /// A recipe step with a market floor: the oracle answered.
    StepPrices { flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>> },
    EvmAddress,
    EvmPrices(evm::EvmArgs),
    SolAddress,
    SolPrices(String),
    RemotePrices(remote::Begin),
    /// Closing: the vault's token balances (then its Intents balances) came back.
    Close { to: AccountId, tokens: Vec<AccountId>, intents: Vec<String> },
    /// Settling fees: the vault's balance of the token.
    Settle { token: AccountId },
    /// A Solana template transaction with a market field: the oracle answered.
    SolTxPrices(String),
}

#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub struct ReadArgs {
    vault: AccountId,
    ctx: Ctx,
    read: Read,
}

/// What a decision produces: an answer for the vault now, or a read whose
/// callback will produce the answer.
pub(crate) enum Reply {
    Answer(Answer),
    Read(Promise),
    /// The vault's last answer: its record is already gone, nothing to store (close.rs).
    Final(Answer),
}

impl From<Answer> for Reply {
    fn from(a: Answer) -> Self {
        Reply::Answer(a)
    }
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Brain {
    /// Only sub-accounts of this factory are vaults, and only it can make them.
    pub factory: AccountId,
    /// Protocol fees and their caps (fees.rs).
    pub fees: fees::Fees,
}

/// A vault talking to the brain: who it is, what it reported, its record.
pub(crate) struct Talk {
    pub vault: AccountId,
    pub ctx: Ctx,
    pub vs: VaultState,
    /// Tgas the vault's rounds need after the pending read's answer (a step's
    /// recipe rounds, a signature's): a read moved to the next round must leave it.
    pub need_after: u64,
}

#[near]
impl Brain {
    #[init]
    /// `fees`: the treasury, the fee admin, the protocol fee and the signature fee
    /// with their caps (fixed here for good). None: no fees, ever.
    pub fn new(factory: AccountId, fees: Option<fees::Fees>) -> Self {
        let fees = fees.unwrap_or_else(|| fees::Fees::none(factory.clone()));
        fees.check();
        Self { factory, fees }
    }

    /// The fee admin moves the fees, within the caps fixed at deployment.
    pub fn set_fee(&mut self, bps: u16, per_signature: U128) {
        require!(env::predecessor_account_id() == self.fees.admin, "fee admin only");
        let mut f = self.fees.clone();
        f.bps = bps;
        f.per_signature = per_signature;
        f.check();
        self.fees = f;
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"fees_set","data":[{{"bps":{bps},"per_signature":"{}"}}]}}"#, per_signature.0));
    }

    pub fn get_fees(&self) -> near_sdk::serde_json::Value {
        let f = &self.fees;
        near_sdk::serde_json::json!({"treasury": f.treasury, "admin": f.admin, "bps": f.bps, "cap_bps": f.cap_bps,
            "per_signature": f.per_signature, "per_signature_cap": f.per_signature_cap})
    }

    /// A request forwarded by a vault (relay `x`, a named door, or `new`).
    pub fn handle(&self) {
        let input = env::input().unwrap_or_default();
        let (ctx, body) = wire::parse(&input);
        let sender = ctx.sender.clone().unwrap_or_else(|| env::panic_str("a request names its sender"));
        let vault = env::predecessor_account_id();
        let req: Req = near_sdk::serde_json::from_slice(body).unwrap_or_else(|e| env::panic_str(&format!("bad request: {e}")));
        match req {
            Req::Init { owner, keeper } => return self.init(vault, ctx, sender, owner, keeper, None),
            Req::InitStealth { owner_key, keeper } => {
                let key = stealth::ed25519(&owner_key);
                return self.init(vault.clone(), ctx, sender, vault, keeper, Some(key));
            }
            // Hooks: anyone can call them with any claims; the answer is always "keep it".
            Req::NftOnTransfer { .. } => return Answer::default().ret("false").send(),
            Req::MtOnTransfer { amounts, .. } => {
                let zeros = vec![r#""0""#; amounts.len()].join(",");
                return Answer::default().ret(&format!("[{zeros}]")).send();
            }
            _ => {}
        }
        let mut t = Talk::open(vault, ctx, HANDLE_TGAS);
        let r = t.request(sender, req);
        t.finish(r);
    }

    /// A vault paying for its record (asked for in an answer).
    #[payable]
    pub fn pay(&mut self) {
        let vault = env::predecessor_account_id();
        let mut vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        vs.paid = U128(vs.paid.0 + env::attached_deposit().as_yoctonear());
        store(&vault, &vs);
    }

    /// A callback the vault's `cb` makes on the results of actions the brain asked for.
    pub fn on_vault(&self) {
        let input = env::input().unwrap_or_default();
        let (ctx, body) = wire::parse(&input);
        require!(ctx.sender.is_none(), "callbacks come from the vault itself");
        let next = Next::try_from_slice(body).unwrap_or_else(|_| env::panic_str("bad callback"));
        let spent = match &next {
            Next::Call { tgas, .. } => *tgas,
            Next::Read { views, gas, .. } => read_tgas(views.len(), *gas) + OWN_TGAS,
            Next::FeeSettled { .. } => fees::GAS_SETTLED.as_tgas(),
            Next::IntentsSigned { .. } => intents::GAS_SIGNED.as_tgas(),
            Next::Exec { tgas, .. } => *tgas,
            Next::EvmSigned(_) | Next::RemoteEvmSigned { .. } => evm::GAS_SIGNED.as_tgas(),
            Next::SolSigned(_) | Next::RemoteSolSigned { .. } => sol::GAS_SIGNED.as_tgas(),
        };
        let mut t = Talk::open(env::predecessor_account_id(), ctx, spent);
        let r = match next {
            Next::Call { id, i, .. } => t.on_call(id, i),
            Next::Read { views, read, gas } => t.read_now(views, read, Gas::from_tgas(gas)),
            Next::FeeSettled { token, amount } => t.on_fee_settled(token, amount),
            Next::IntentsSigned { message, nonce } => t.on_intents_signed(message, nonce),
            Next::Exec { method, args, .. } => t.run_command(method, args),
            Next::EvmSigned(tx) => t.on_evm_signed(tx),
            Next::SolSigned(hash) => t.on_sol_signed(hash),
            Next::RemoteEvmSigned { flow_id, node, tx } => t.on_remote_evm_signed(flow_id, node, tx),
            Next::RemoteSolSigned { flow_id, node, hash } => t.on_remote_sol_signed(flow_id, node, hash),
        };
        t.finish(r);
    }

    /// The brain's own reads came back.
    #[private]
    pub fn on_read(&self, #[serializer(borsh)] a: ReadArgs) {
        let vs = load(&a.vault).unwrap_or_else(|| env::panic_str("not a vault"));
        let mut t = Talk { vault: a.vault, ctx: a.ctx, vs, need_after: 0 };
        let r = match a.read {
            Read::Measured { id, before } => t.on_measured(id, before),
            Read::Price { flow_id, node, by_owner, per, revealed } => t.on_price(flow_id, node, by_owner, per, revealed),
            Read::Fund { flow_id, node, amount } => t.on_fund(flow_id, node, amount),
            Read::Withdraw { token, amount, to, via_intents } => t.on_withdraw(token, amount, to, via_intents),
            Read::IntentsKey => t.on_intents_key(),
            Read::IntentsSalt(p) => t.on_intents_salt(p),
            Read::StepPrices { flow_id, node, price, by_owner, revealed } => t.on_step_prices(flow_id, node, price, by_owner, revealed),
            Read::EvmAddress => t.on_evm_address(),
            Read::EvmPrices(a) => t.on_evm_prices(a),
            Read::SolAddress => t.on_sol_address(),
            Read::SolPrices(m) => t.on_sol_prices(m),
            Read::RemotePrices(b) => t.on_remote_prices(b),
            Read::Close { to, tokens, intents } => t.on_close(to, tokens, intents, &self.factory),
            Read::Settle { token } => t.on_settle(token),
            Read::SolTxPrices(m) => t.on_sol_tx_prices(m),
        };
        t.finish(r);
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
        U128(allocated(&must_load(&vault), &token))
    }

    pub fn get_limits(&self, vault: AccountId) -> (Vec<Limit>, Vec<PendingLimit>) {
        let vs = must_load(&vault);
        (vs.limits, vs.pending)
    }

    pub fn get_intents(&self, vault: AccountId) -> Option<IntentsConfig> {
        must_load(&vault).intents
    }

    pub fn get_oracle(&self, vault: AccountId) -> Option<OracleConfig> {
        must_load(&vault).oracle
    }

    pub fn get_evm(&self, vault: AccountId) -> Option<evm::EvmConfig> {
        must_load(&vault).evm
    }

    pub fn get_sol(&self, vault: AccountId) -> Option<sol::SolConfig> {
        must_load(&vault).sol
    }

    pub fn get_remote_pending(&self, vault: AccountId, flow_id: u32, node: u16) -> Option<remote::Pending> {
        must_load(&vault).remote.into_iter().find(|p| p.flow == flow_id && p.node == node)
    }

    /// A stealth vault's owner key (`ed25519:<base58>`).
    pub fn get_owner_key(&self, vault: AccountId) -> Option<String> {
        must_load(&vault).stealth.map(|s| format!("ed25519:{}", near_sdk::bs58::encode(s.key).into_string()))
    }

    /// The last nonce a stealth command used (the next needs a bigger one).
    pub fn get_nonce(&self, vault: AccountId) -> U64 {
        U64(must_load(&vault).stealth.map_or(0, |s| s.nonce))
    }

    /// A node's fingerprint (ask your own RPC: whoever answers sees the node).
    pub fn fingerprint_of(&self, node: Node<Recipe>, salt: String) -> String {
        fingerprint(&node, &salt)
    }

    /// What this vault's record costs and what it has paid.
    pub fn get_storage(&self, vault: AccountId) -> (U128, U128) {
        let vs = must_load(&vault);
        (U128(record_cost(&vault, &vs)), vs.paid)
    }
}

impl Brain {
    /// The factory made this vault (a direct sub-account of the factory, and the
    /// factory itself called the vault's `new`): give it an empty record.
    fn init(&self, vault: AccountId, ctx: Ctx, sender: AccountId, owner: AccountId, keeper: Option<AccountId>, key: Option<[u8; 32]>) {
        require!(sender == self.factory, "vaults are made by the factory");
        let name = vault.as_str().strip_suffix(&format!(".{}", self.factory)).unwrap_or("");
        require!(!name.is_empty() && !name.contains('.'), "not one of the factory's vaults");
        require!(load(&vault).is_none(), "already a vault");
        let vs = VaultState {
            owner,
            keeper,
            flows: Vec::new(),
            busy: None,
            trusted: Vec::new(),
            limits: Vec::new(),
            pending: Vec::new(),
            intents: None,
            oracle: None,
            evm: None,
            sol: None,
            remote: Vec::new(),
            stealth: key.map(|key| stealth::Stealth { key, nonce: 0 }),
            paid: U128(0),
            next_step: 0,
            auto_close: None,
            crosschain: false,
            keeper_fee: U128(0),
            owed_fees: Vec::new(),
        };
        Talk { vault, ctx, vs, need_after: 0 }.save(Answer::default()).send();
    }
}

impl Talk {
    /// Load the vault's record. `spent_tgas`: what the call now running took from
    /// the vault's round, so `ctx.gas_left` becomes what the vault's NEXT round has.
    fn open(vault: AccountId, mut ctx: Ctx, spent_tgas: u64) -> Self {
        let vs = load(&vault).unwrap_or_else(|| env::panic_str("not a vault"));
        ctx.gas_left = ctx.gas_left.saturating_sub(spent_tgas * 1_000_000_000_000);
        Talk { vault, ctx, vs, need_after: 0 }
    }

    pub(crate) fn finish(self, r: Reply) {
        match r {
            Reply::Answer(a) => self.save(a).send(),
            // The read's callback saves again and asks for any payment then.
            Reply::Read(p) => {
                store(&self.vault, &self.vs);
                let _ = p.as_return();
            }
            Reply::Final(a) => a.send(),
        }
    }

    /// The brain reads `views` itself (in parallel), then `on_read` decides with `read`
    /// (given `gas`). Right here if this receipt has the gas for it; otherwise the
    /// answer asks the vault for one more round (`on_vault` Read) sized for exactly
    /// this read, paid from what the vault's caller attached. That is what lets the
    /// relay's first round stay small: only reads that fit it run in it.
    pub(crate) fn read(&self, views: Vec<(AccountId, String, String)>, read: Read, gas: Gas) -> Reply {
        let need = read_tgas(views.len(), gas.as_tgas());
        let here = (env::prepaid_gas().as_gas() - env::used_gas().as_gas()) / 1_000_000_000_000;
        if here >= need + FINISH_TGAS {
            return self.read_now(views, read, gas);
        }
        let round = need + OWN_TGAS;
        let total = round + RELAY_TGAS + self.need_after;
        require!(self.ctx.gas_left / 1_000_000_000_000 >= total, format!("attach at least {} Tgas", total + HANDLE_TGAS + X_TGAS));
        let next = near_sdk::borsh::to_vec(&Next::Read { views, read, gas: gas.as_tgas() }).unwrap();
        Reply::Answer(Answer::default().then("on_vault", next, Gas::from_tgas(round)))
    }

    pub(crate) fn read_now(&self, views: Vec<(AccountId, String, String)>, read: Read, gas: Gas) -> Reply {
        let mut joint: Option<Promise> = None;
        for (to, method, args) in views {
            let p = Promise::new(to).function_call(method, args.into_bytes(), NearToken::from_yoctonear(0), GAS_VIEW);
            joint = Some(match joint {
                None => p,
                Some(j) => j.and(p),
            });
        }
        let cb = Brain::ext(env::current_account_id()).with_static_gas(gas).on_read(ReadArgs { vault: self.vault.clone(), ctx: self.ctx.clone(), read });
        Reply::Read(match joint {
            Some(j) => j.then(cb),
            None => cb,
        })
    }

    pub(crate) fn balance_view(&self, token: &AccountId) -> (AccountId, String, String) {
        (token.clone(), "ft_balance_of".into(), format!(r#"{{"account_id":"{}"}}"#, self.vault))
    }

    fn request(&mut self, sender: AccountId, req: Req) -> Reply {
        let owner = sender == self.vs.owner;
        let keeper = Some(&sender) == self.vs.keeper.as_ref();
        let owner_only = || require!(owner, "owner only");
        let crew_only = || require!(owner || keeper, "owner or keeper only");
        // Anything that touches another chain marks the vault for good (close.rs).
        let remote_node = |n: &Node<Recipe>| n.action.remote.is_some() || remote::is_remote(&n.input);
        if match &req {
            Req::EvmSetup { .. } | Req::SolSetup { .. } | Req::IntentsSetup { .. } | Req::StepRemote { .. } => true,
            Req::SaveFlow { nodes, .. } | Req::RevealFlow { nodes, .. } => nodes.iter().any(remote_node),
            Req::SaveHiddenFlow { inputs, .. } => inputs.iter().any(|t| remote::is_remote(t)),
            _ => false,
        } {
            self.vs.crosschain = true;
        }
        match req {
            Req::Close { confirm_remote, to } => return self.close(owner, confirm_remote, to),
            Req::Init { .. } | Req::InitStealth { .. } | Req::NftOnTransfer { .. } | Req::MtOnTransfer { .. } => env::panic_str("not here"),
            Req::Step { flow_id, node } => {
                require!(!self.flow(flow_id).is_hidden(), "a hidden flow runs with step_hidden");
                let n = self.flow(flow_id).nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
                return self.begin_step(&sender, flow_id, node, n, false);
            }
            Req::StepHidden { flow_id, node, reveal, salt } => return self.step_hidden(&sender, flow_id, node, reveal, salt),
            Req::IntentsSignSwap { token_in, token_out, amount_in, min_out, valid_secs } => {
                crew_only();
                return self.intents_sign_swap(owner, token_in, token_out, amount_in.0, min_out.0, valid_secs);
            }
            Req::IntentsWithdraw { token, amount } => {
                crew_only();
                return self.intents_withdraw(token, amount.0).into();
            }
            Req::EvmCall { name, values, nonce, max_fee, max_tip, gas } => {
                crew_only();
                return self.evm_call(evm::EvmArgs { name, values, nonce, max_fee, max_tip, gas });
            }
            Req::SolSignSwap { message } => {
                crew_only();
                return self.sol_sign_swap(message);
            }
            Req::SolSignTx { message } => {
                crew_only();
                return self.sol_sign_tx(message);
            }
            Req::StepRemote { flow_id, node, args, reveal, salt } => return self.step_remote(&sender, flow_id, node, args, reveal, salt),
            Req::RemoteReport { flow_id, node, out } => return self.remote_report(&sender, flow_id, node, out).into(),
            Req::ApplyLimit { token } => self.apply_limit(token),
            Req::SettleFees { token } => return self.settle_fees(token),
            Req::Exec { method, args, nonce, expires_ns, tgas, signature } => return self.exec(method, args, nonce.0, expires_ns.0, tgas, signature).into(),
            other => {
                owner_only();
                return self.owner_request(other);
            }
        }
        Answer::default().into()
    }

    /// Everything only the owner may ask.
    fn owner_request(&mut self, req: Req) -> Reply {
        match req {
            Req::SetKeeper { keeper, keeper_fee } => self.set_keeper(keeper, keeper_fee),
            Req::Trust { add, remove } => self.trust(add, remove),
            Req::SaveFlow { name, nodes } => {
                let id = self.save_flow(name, nodes);
                return Answer::default().ret(&id.to_string()).into();
            }
            Req::DeleteFlow { flow_id } => {
                self.idle();
                let f = self.flow(flow_id);
                require!(f.ledger.holdings_inputs(&f.inputs).is_empty(), "drain the flow first");
                self.vs.flows[flow_id as usize] = None;
            }
            Req::Fund { flow_id, node, amount } => {
                self.idle();
                require!(amount.0 > 0, "nothing to fund");
                let input = self.flow(flow_id).inputs.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
                if remote::is_remote(&input) {
                    // On another chain nobody here can read the balance: the owner's word.
                    self.credit(flow_id, node, amount.0);
                } else if input == NEAR {
                    require!(amount.0 <= self.free_near(), "not enough free NEAR");
                    self.credit(flow_id, node, amount.0);
                } else {
                    let token: AccountId = input.parse().unwrap();
                    return self.read(vec![self.balance_view(&token)], Read::Fund { flow_id, node, amount: amount.0 }, GAS_READ);
                }
            }
            Req::Drain { flow_id } => {
                let f = self.flow_mut(flow_id);
                f.ledger.drain_inputs(&f.inputs).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
            }
            Req::Resume { flow_id, node } => {
                self.flow_mut(flow_id).ledger.resume(node).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
            }
            Req::Unstick {} => self.unstick(),
            Req::Withdraw { token, amount } => {
                require!(self.vs.stealth.is_none(), "a stealth vault withdraws with withdraw_to");
                let to = self.vs.owner.clone();
                return self.withdraw(token, amount.0, to, false);
            }
            Req::WithdrawTo { token, amount, to, via_intents } => {
                require!(to != self.vault, "to: another account");
                return self.withdraw(token, amount.0, to, via_intents);
            }
            Req::Act { target, method, json_args, one_yocto } => {
                self.idle();
                let t = self.trusted(&target).unwrap_or_else(|| env::panic_str("untrusted: contract"));
                require!(!NEVER_CALL.contains(&method.as_str()), "that method moves funds: use withdraw");
                require!(t.methods.iter().any(|m| *m == method), "untrusted: method");
                return Answer::default().call(&target, &method, json_args.as_bytes(), one_yocto as u128, GAS_ACT).into();
            }
            Req::SetLimit { token, max, window_ns } => self.set_limit(token, max, window_ns),
            Req::SaveHiddenFlow { name, inputs, fingerprints } => {
                let id = self.save_hidden_flow(name, inputs, fingerprints);
                return Answer::default().ret(&id.to_string()).into();
            }
            Req::HideFlow { flow_id, fingerprints } => self.hide_flow(flow_id, fingerprints),
            Req::RevealFlow { flow_id, nodes, salts } => self.reveal_flow(flow_id, nodes, salts),
            Req::SetOwnerKey { owner_key } => self.set_owner_key(owner_key),
            Req::SetAutoClose { on, to } => self.set_auto_close(on, to),
            Req::IntentsSetup { verifier, signer } => return self.intents_setup(verifier, signer),
            Req::IntentsAllow { pair } => self.intents_allow(pair),
            Req::OracleSetup { config } => self.oracle_setup(config),
            Req::EvmSetup { signer, chain_id, max_fee, max_tip, gas_cap } => return self.evm_setup(signer, chain_id, max_fee, max_tip, gas_cap),
            Req::EvmTemplate { template } => self.evm_template(template),
            Req::EvmTemplateRemove { name } => self.evm_template_remove(name),
            Req::SolSetup { signer, max_cu_price, max_cu_limit } => return self.sol_setup(signer, max_cu_price, max_cu_limit),
            Req::SolAllow { pair } => self.sol_allow(pair),
            Req::SolOwnerWallet { wallet } => self.sol_owner_wallet(wallet),
            Req::SolTemplate { template } => self.sol_template(template),
            Req::SolTemplateRemove { name } => self.sol_template_remove(name),
            _ => env::panic_str("not here"),
        }
        Answer::default().into()
    }

    /// Free balance out: native NEAR at once, a token after reading the balance.
    fn withdraw(&mut self, token: String, amount: u128, to: AccountId, via_intents: bool) -> Reply {
        self.idle();
        if token == NEAR {
            require!(!via_intents, "wrap NEAR first: Intents takes tokens");
            require!(amount <= self.free_near(), "not enough free NEAR");
            return Answer::default().transfer(&to, amount).into();
        }
        let t: AccountId = token.parse().unwrap_or_else(|_| env::panic_str("bad token"));
        self.read(vec![self.balance_view(&t)], Read::Withdraw { token: t, amount, to, via_intents }, GAS_READ)
    }

    fn on_withdraw(&mut self, token: AccountId, amount: u128, to: AccountId, via_intents: bool) -> Reply {
        self.idle();
        let balance = amount_result(0).unwrap_or_else(|| env::panic_str("balance unknown"));
        let free = balance.saturating_sub(self.allocated(token.as_str()));
        require!(amount <= free, "not enough free balance");
        if via_intents {
            let verifier = self.vs.intents.as_ref().map(|c| c.verifier.clone()).unwrap_or_else(|| env::panic_str("run intents_setup first"));
            // The verifier credits the account named in `msg`, not the sender.
            let args = near_sdk::serde_json::json!({"receiver_id": verifier, "amount": amount.to_string(), "msg": to}).to_string();
            return Answer::default().call(&token, "ft_transfer_call", args.as_bytes(), 1, Gas::from_tgas(40)).into();
        }
        let args = format!(r#"{{"receiver_id":"{to}","amount":"{amount}"}}"#);
        Answer::default().call(&token, "ft_transfer", args.as_bytes(), 1, GAS_FT_TRANSFER).into()
    }

    fn on_fund(&mut self, flow_id: u32, node: u16, amount: u128) -> Reply {
        self.idle();
        let token = self.flow(flow_id).inputs[node as usize].clone();
        let balance = amount_result(0).unwrap_or_else(|| env::panic_str("balance unknown"));
        let free = balance.saturating_sub(self.allocated(&token));
        require!(amount <= free, "not enough free balance");
        self.credit(flow_id, node, amount);
        Answer::default().into()
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
        for (i, n) in nodes.iter().enumerate() {
            self.check_node(n);
            self.check_gas(i, n);
        }
        self.add_flow(FlowRec { name, ledger: Ledger::new(nodes.len()), inputs: flow::inputs(&nodes), hidden: Vec::new(), nodes })
    }

    /// Trusted, and nothing the brain doesn't run yet.
    pub(crate) fn check_node(&self, n: &Node<Recipe>) {
        let why = if n.action.remote.is_some() { self.remote_untrusted(n) } else { self.untrusted(n) };
        if let Some(why) = why {
            env::panic_str(&format!("untrusted: {why}"));
        }
    }

    pub(crate) fn add_flow(&mut self, rec: FlowRec) -> u32 {
        if let Some(i) = self.vs.flows.iter().position(Option::is_none) {
            self.vs.flows[i] = Some(rec);
            return i as u32;
        }
        require!(self.vs.flows.len() < MAX_FLOWS, "too many flows");
        self.vs.flows.push(Some(rec));
        (self.vs.flows.len() - 1) as u32
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

    /// A trusted contract: never the vault itself, never the brain.
    pub(crate) fn trusted(&self, a: &AccountId) -> Option<&Trust> {
        let brain = env::current_account_id();
        self.vs.trusted.iter().find(|t| t.contract == *a && *a != self.vault && *a != brain)
    }

    /// Why node `n` may not run, if it may not (same rules as the vault).
    pub(crate) fn untrusted(&self, n: &Node<Recipe>) -> Option<String> {
        let contract_ok = |a: &AccountId| self.trusted(a).map_or(Some(format!("contract {a}")), |_| None);
        let method_ok = |a: &AccountId, m: &str| match self.trusted(a) {
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
        // A hidden flow's routes aren't known, so its owner may fund any node.
        let r = if f.is_hidden() { f.ledger.fund_node(&f.inputs[node as usize], node, amount) } else { f.ledger.fund(&f.nodes, node, amount) };
        r.unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
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
    /// also makes the vault pay the difference (last action, so callbacks still
    /// find their own results at the indices they expect).
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
        let sep = if data.is_empty() { "" } else { "," };
        env::log_str(&format!(r#"EVENT_JSON:{{"standard":"flows","version":"1.0.0","event":"{event}","data":[{{"vault":"{}"{sep}{data}}}]}}"#, self.vault));
    }
}

/// A promise result (at most `max` bytes), or None if it failed or isn't there.
pub(crate) fn result(i: u64, max: usize) -> Option<Vec<u8>> {
    if i >= env::promise_results_count() {
        return None;
    }
    env::promise_result_checked(i, max).ok()
}

pub(crate) fn amount_result(i: u64) -> Option<u128> {
    result(i, 1024).and_then(|v| near_sdk::serde_json::from_slice::<U128>(&v).ok()).map(|a| a.0)
}

pub(crate) fn key(vault: &AccountId) -> Vec<u8> {
    [b"v:".as_slice(), vault.as_bytes()].concat()
}

fn load(vault: &AccountId) -> Option<VaultState> {
    env::storage_read(&key(vault)).map(|b| VaultState::try_from_slice(&b).unwrap_or_else(|_| env::panic_str("bad record")))
}

fn must_load(vault: &AccountId) -> VaultState {
    load(vault).unwrap_or_else(|| env::panic_str("not a vault"))
}

fn store(vault: &AccountId, vs: &VaultState) {
    env::storage_write(&key(vault), &borsh::to_vec(vs).unwrap());
}

fn record_cost(vault: &AccountId, vs: &VaultState) -> u128 {
    let bytes = RECORD_OVERHEAD + key(vault).len() as u64 + borsh::to_vec(vs).unwrap().len() as u64;
    bytes as u128 * env::storage_byte_cost().as_yoctonear()
}

/// What isn't free: what the flows hold, and protocol fees set aside (fees.rs).
pub(crate) fn allocated(vs: &VaultState, token: &str) -> u128 {
    let owed = vs.owed_fees.iter().find(|(t, _)| t == token).map_or(0, |(_, a)| a.0);
    vs.flows.iter().flatten().map(|f| f.ledger.holdings_inputs(&f.inputs).get(token).copied().unwrap_or(0)).sum::<u128>() + owed
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
