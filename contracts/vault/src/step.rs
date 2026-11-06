//! Running one step of a flow, across several blocks:
//!
//!   [price view] -> balances before -> call 1 -> call 2 ... -> balances after -> finish
//!
//! NEAR calls are asynchronous: each arrow is a separate receipt, and any call
//! can fail on its own. The vault is marked busy for the whole step, so the
//! balance difference measured at the end belongs to this step only. A failed
//! call skips the rest and goes straight to "balances after": whatever input
//! came back returns to the node, whatever output arrived becomes free.

use near_contract_standards::fungible_token::core::ext_ft_core;
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas, GasWeight, NearToken, Promise};

use flow::Node;

use crate::{Call, Deposit, Recipe, Vault, VaultExt, NEAR, PRICE_SCALE};

const GAS_VIEW: Gas = Gas::from_tgas(5);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);
/// What a step needs on top of its recipe's calls: our views and callbacks.
const STEP_OVERHEAD_TGAS: u64 = 60;
const PER_CALL_OVERHEAD_TGAS: u64 = 15;
/// Reading two feeds (with backups) and the callback that follows.
const ORACLE_OVERHEAD_TGAS: u64 = 40;

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct StepCtx {
    pub flow: u32,
    pub node: u16,
    pub amount: U128,
    pub started_ns: u64,
    pub in_before: Option<U128>,
    pub out_before: Option<U128>,
    /// NEAR attached as the input by calls that succeeded.
    pub native_used: U128,
    pub failed: bool,
    /// A hidden flow's node, as revealed to run this step.
    pub revealed: Option<Node<Recipe>>,
    /// The market floor read from the oracle for this step (per unit of input, 1e24).
    pub market_per_in: Option<U128>,
}

#[near]
impl Vault {
    /// Run node `node` of flow `flow_id`, if its trigger has fired. Owner or keeper.
    pub fn step(&mut self, flow_id: u32, node: u16) -> Promise {
        require!(!self.flow(flow_id).is_hidden(), "a hidden flow runs with step_hidden");
        let n = self.flow(flow_id).nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
        self.begin_step(flow_id, node, n, false)
    }

    #[private]
    pub fn on_price(&mut self, flow_id: u32, node: u16, by_owner: bool, per: U128, revealed: Option<Node<Recipe>>) -> Promise {
        let raw = result(0).unwrap_or_else(|| env::panic_str("price source failed"));
        let amount: U128 = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("price source returned no amount"));
        require!(per.0 > 0, "bad price source");
        let price = crate::mul_div(amount.0, PRICE_SCALE, per.0);
        self.start(flow_id, node, Some(price), by_owner, revealed)
    }

    /// The oracle answered for a recipe with a market floor: check the prices, then start.
    #[private]
    pub fn on_step_prices(&mut self, flow_id: u32, node: u16, price: Option<U128>, by_owner: bool, revealed: Option<Node<Recipe>>) -> Promise {
        let m = revealed.as_ref().unwrap_or_else(|| &self.flow(flow_id).nodes[node as usize]).action.market.clone().unwrap();
        let p = self.oracle_prices(&[m.sell.clone(), m.buy.clone()], 0);
        let per_in = crate::oracle::market_per_in(&m, p[0], p[1], PRICE_SCALE);
        self.emit("market_floor", &format!(r#"{{"flow":{flow_id},"node":{node},"sell":"{}","buy":"{}","min_out_per_in":"{per_in}"}}"#, m.sell, m.buy));
        self.start_now(flow_id, node, price.map(|p| p.0), by_owner, revealed, Some(per_in))
    }
}

impl Vault {
    /// Run node `n` (stored, or `hidden`: revealed by the caller and already checked).
    pub(crate) fn begin_step(&mut self, flow_id: u32, node: u16, n: Node<Recipe>, hidden: bool) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        require!(self.busy.is_none(), "a step is running");
        // Refuse up front if the caller attached too little gas: a step that runs out
        // halfway would leave the vault busy (found on testnet).
        let need: u64 = n.action.calls.iter().map(|c| match c { Call::Ft { tgas, .. } | Call::Fn { tgas, .. } => *tgas + PER_CALL_OVERHEAD_TGAS }).sum::<u64>()
            + STEP_OVERHEAD_TGAS
            + if n.action.market.is_some() { ORACLE_OVERHEAD_TGAS } else { 0 };
        require!(env::prepaid_gas().as_tgas() >= need, format!("attach at least {need} Tgas for this step"));
        // Trust can be withdrawn after a flow was saved: check again before every step.
        if let Some(why) = self.untrusted(&n) {
            env::panic_str(&format!("untrusted: {why}"));
        }
        if matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
            let src = n.action.price.clone().unwrap_or_else(|| env::panic_str("price trigger without a price source"));
            return Promise::new(src.contract)
                .function_call_weight(src.method, src.args.into_bytes(), NearToken::from_yoctonear(0), GAS_VIEW, GasWeight(0))
                .then(self.cb().on_price(flow_id, node, by_owner, src.per, hidden.then_some(n)));
        }
        self.start(flow_id, node, None, by_owner, hidden.then_some(n))
    }
}

#[near]
impl Vault {
    #[private]
    pub fn on_measured(&mut self, before: bool) -> Promise {
        let mut ctx = self.busy.clone().unwrap_or_else(|| env::panic_str("no step"));
        let (input, out) = self.tokens(&ctx);
        let mut results = (0..env::promise_results_count()).map(|i| result(i).and_then(|v| near_sdk::serde_json::from_slice::<U128>(&v).ok()));
        let in_bal = if input.is_some() { Some(results.next().flatten()) } else { None };
        let out_bal = if out.is_some() { Some(results.next().flatten()) } else { None };
        if before {
            if in_bal == Some(None) || out_bal == Some(None) {
                // Couldn't even read a balance: nothing happened, give the input back.
                return self.end(Err(ctx.amount.0), 0, false);
            }
            ctx.in_before = in_bal.flatten();
            ctx.out_before = out_bal.flatten();
            self.busy = Some(ctx);
            return self.run_call(0);
        }
        let in_used = match (ctx.in_before, in_bal) {
            (Some(b), Some(Some(a))) => b.0.saturating_sub(a.0).min(ctx.amount.0),
            (Some(_), _) => ctx.amount.0, // unknown: assume spent (the rest shows up as free)
            (None, _) => ctx.native_used.0,
        };
        let out_amount = match (ctx.out_before, out_bal) {
            (Some(b), Some(Some(a))) => a.0.saturating_sub(b.0),
            _ => 0,
        };
        let left = ctx.amount.0 - in_used;
        // A call can "succeed" without doing anything: a token transfer whose receiver
        // refused it (e.g. a swap below its minimum) just refunds. Nothing spent = failed.
        let collector = self.step_node(&ctx).input == flow::NO_INPUT;
        if ctx.failed || (in_used == 0 && !collector) {
            self.end(Err(left), out_amount, false)
        } else {
            self.end(Ok(out_amount), in_used, true)
        }
    }

    #[private]
    pub fn on_call(&mut self, i: u8) -> Promise {
        let mut ctx = self.busy.clone().unwrap_or_else(|| env::panic_str("no step"));
        let calls = self.recipe_calls(&ctx);
        let ok = match (&calls[i as usize], result(0)) {
            (_, None) => false,
            // ft_transfer_call "succeeds" even when the receiver refuses and refunds
            // everything: it returns the amount actually used. Zero used = refused,
            // and the rest of the recipe must not run (e.g. never release collateral
            // after a repay that didn't happen).
            (Call::Ft { .. }, Some(raw)) => near_sdk::serde_json::from_slice::<U128>(&raw).map_or(false, |used| used.0 > 0),
            (Call::Fn { .. }, Some(_)) => true,
        };
        if ok {
            if matches!(calls[i as usize], Call::Fn { deposit: Deposit::Input, .. }) {
                ctx.native_used = ctx.amount;
            }
            self.busy = Some(ctx);
            if (i as usize) + 1 < calls.len() {
                return self.run_call(i + 1);
            }
        } else {
            ctx.failed = true;
            self.busy = Some(ctx);
        }
        self.measure(false)
    }
}

/// A promise's result (at most 1 KB: we only read amounts), or None if it failed.
fn result(i: u64) -> Option<Vec<u8>> {
    env::promise_result_checked(i, 1024).ok()
}

impl Vault {
    fn cb(&self) -> VaultExt {
        Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).with_unused_gas_weight(1)
    }

    /// Start the step; a recipe with a market floor reads the oracle first.
    fn start(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>) -> Promise {
        let market = revealed.as_ref().unwrap_or_else(|| &self.flow(flow_id).nodes[node as usize]).action.market.clone();
        match market {
            None => self.start_now(flow_id, node, price, by_owner, revealed, None),
            Some(m) => self
                .oracle_fetch(&[m.sell.clone(), m.buy.clone()])
                .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).with_unused_gas_weight(1).on_step_prices(flow_id, node, price.map(U128), by_owner, revealed)),
        }
    }

    fn start_now(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>, market_per_in: Option<u128>) -> Promise {
        require!(self.busy.is_none(), "a step is running");
        let now = env::block_timestamp();
        let f = self.flow_mut(flow_id);
        let n = revealed.as_ref().unwrap_or_else(|| &f.nodes[node as usize]);
        let amount = f.ledger.begin_node(n, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let input = n.input.clone();
        self.charge(&input, amount);
        self.busy = Some(StepCtx {
            flow: flow_id,
            node,
            amount: U128(amount),
            started_ns: now,
            in_before: None,
            out_before: None,
            native_used: U128(0),
            failed: false,
            revealed,
            market_per_in: market_per_in.map(U128),
        });
        self.measure(true)
    }

    /// The node a step is running: revealed for it (hidden flow) or stored.
    pub(crate) fn step_node(&self, ctx: &StepCtx) -> Node<Recipe> {
        ctx.revealed.clone().unwrap_or_else(|| self.flow(ctx.flow).nodes[ctx.node as usize].clone())
    }

    /// (input token if it's a token, output token if any)
    fn tokens(&self, ctx: &StepCtx) -> (Option<AccountId>, Option<AccountId>) {
        let n = &self.step_node(ctx);
        let input = if n.input == NEAR || n.input == flow::NO_INPUT { None } else { n.input.parse().ok() };
        (input, n.action.out.clone())
    }

    fn recipe_calls(&self, ctx: &StepCtx) -> Vec<Call> {
        self.step_node(ctx).action.calls
    }

    /// Read the vault's balances of the step's tokens, then continue in `on_measured`.
    fn measure(&mut self, before: bool) -> Promise {
        let ctx = self.busy.clone().unwrap();
        let (input, out) = self.tokens(&ctx);
        let me = env::current_account_id();
        let views: Vec<Promise> =
            // Weight 0: the views get exactly GAS_VIEW, and all the rest goes to the callback,
            // which must still launch the recipe's calls (testnet: it ran dry when they shared it).
            input.into_iter().chain(out).map(|t| ext_ft_core::ext(t).with_static_gas(GAS_VIEW).with_unused_gas_weight(0).ft_balance_of(me.clone())).collect();
        let mut it = views.into_iter();
        let joint = match it.next() {
            Some(first) => it.fold(first, |acc, p| acc.and(p)),
            // Nothing to measure (NEAR in, nothing out): a no-op view keeps the chain uniform.
            None => Promise::new(me.clone()).function_call_weight("get_owner".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), GAS_VIEW, GasWeight(0)),
        };
        joint.then(self.cb().on_measured(before))
    }

    fn run_call(&mut self, i: u8) -> Promise {
        let mut ctx = self.busy.clone().unwrap();
        let node = self.step_node(&ctx);
        // Not enough gas left to launch this call and come back: close the step as
        // failed (measuring what moved) rather than leave the vault stuck.
        let tgas = match &node.action.calls[i as usize] { Call::Ft { tgas, .. } | Call::Fn { tgas, .. } => *tgas };
        let left = env::prepaid_gas().as_tgas().saturating_sub(env::used_gas().as_tgas());
        if left < tgas + 2 * PER_CALL_OVERHEAD_TGAS + 20 {
            ctx.failed = true;
            self.busy = Some(ctx);
            return self.measure(false);
        }
        let amount = ctx.amount.0;
        let min_out = crate::mul_div(amount, min_per_in(&node.action, &ctx), PRICE_SCALE);
        let scaled = node.action.scale_per_in.map_or(0, |m| crate::mul_div(amount, m.0, PRICE_SCALE));
        let fill = |s: &str| {
            s.replace("{amount}", &amount.to_string())
                .replace("{scaled}", &scaled.to_string())
                .replace("{min_out}", &min_out.to_string())
                .replace("{owner}", self.owner.as_str())
                .replace("{vault}", env::current_account_id().as_str())
        };
        let call = match &node.action.calls[i as usize] {
            Call::Ft { receiver, msg, tgas } => ext_ft_core::ext(node.input.parse().unwrap())
                .with_attached_deposit(NearToken::from_yoctonear(1))
                .with_static_gas(Gas::from_tgas(*tgas))
                .with_unused_gas_weight(0)
                .ft_transfer_call(receiver.clone(), U128(amount), None, fill(msg)),
            Call::Fn { contract, method, args, deposit, tgas } => {
                let attach = match deposit {
                    Deposit::None => 0,
                    Deposit::OneYocto => 1,
                    Deposit::Input => amount,
                    Deposit::Fixed { yocto } => yocto.0,
                };
                Promise::new(contract.clone()).function_call_weight(
                    method.clone(),
                    fill(args).into_bytes(),
                    NearToken::from_yoctonear(attach),
                    Gas::from_tgas(*tgas),
                    GasWeight(0),
                )
            }
        };
        call.then(self.cb().on_call(i))
    }

    /// Close the step. `result`: Ok(output) or Err(input that came back).
    /// `extra` is the input used (success) or stray output (failure), for the event.
    fn end(&mut self, result: Result<u128, u128>, extra: u128, ok: bool) -> Promise {
        let ctx = self.busy.take().unwrap();
        let node = self.step_node(&ctx);
        let (min, promises_output) = {
            let n = &node;
            // A collector finding nothing yet is normal; only spenders must get something back.
            (crate::mul_div(extra, min_per_in(&n.action, &ctx), PRICE_SCALE), n.action.out.is_some() && n.input != flow::NO_INPUT)
        };
        let f = self.flow_mut(ctx.flow);
        f.ledger.finish_node(&node, ctx.node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let mut paused = false;
        if let Ok(out) = result {
            // Less than its minimum, or nothing at all when it promised an output
            // (e.g. a message that sends the proceeds elsewhere): stop and wait for the owner.
            if out < min || (promises_output && out == 0) {
                f.ledger.pause(ctx.node).ok();
                paused = true;
            }
        }
        let (r, v) = match result {
            Ok(v) | Err(v) => (if ok { "out" } else { "returned" }, v),
        };
        self.emit(
            if ok { "step" } else { "step_failed" },
            &format!(
                r#"{{"flow":{},"node":{},"amount":"{}","{r}":"{v}","{}":"{extra}","paused":{paused}}}"#,
                ctx.flow,
                ctx.node,
                ctx.amount.0,
                if ok { "used" } else { "stray_out" }
            ),
        );
        // Promise chains need a promise; a self-view is the cheapest no-op.
        Promise::new(env::current_account_id()).function_call_weight("get_owner".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), GAS_VIEW, GasWeight(0))
    }
}

/// The least output per unit of input (1e24): the owner's floor or the market's, whichever is higher.
fn min_per_in(r: &Recipe, ctx: &StepCtx) -> u128 {
    r.min_out_per_in.map_or(0, |m| m.0).max(ctx.market_per_in.map_or(0, |m| m.0))
}
