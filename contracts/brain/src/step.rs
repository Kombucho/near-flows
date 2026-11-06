//! One step of a flow, as in vault/src/step.rs, but every arrow is a round
//! trip: the brain answers, the vault executes, the vault's callback asks the
//! brain again with the results.
//!
//!   [price view] -> balances before -> call 1 -> ... -> balances after -> finish
//!
//! Each callback carries the step's id: a late callback from a step that was
//! cleared (`unstick`) can't land in the next step.

use near_sdk::borsh::{self, BorshDeserialize, BorshSerialize};
use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use flow::Node;

use crate::wire::Answer;
use crate::{amount_result, callback, mul_div, result, Brain, BrainExt, Call, Deposit, Recipe, Talk, GAS_BRAIN_CB, GAS_VIEW, NEAR, PRICE_SCALE};

/// A relay round: the brain's callback and the vault's `cb`, with receipts.
const ROUND_TGAS: u64 = 30;
const VIEW_TGAS: u64 = 5;

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct StepCtx {
    pub id: u64,
    pub flow: u32,
    pub node: u16,
    pub amount: U128,
    pub started_ns: u64,
    pub in_before: Option<U128>,
    pub out_before: Option<U128>,
    pub native_used: U128,
    pub failed: bool,
}

#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) struct PriceArgs {
    flow_id: u32,
    node: u16,
    by_owner: bool,
    per: u128,
}

#[derive(BorshSerialize, BorshDeserialize)]
#[borsh(crate = "near_sdk::borsh")]
pub(crate) struct StepArgs {
    id: u64,
    /// on_measured: before (1) or after (0). on_call: the call's index.
    at: u8,
}

/// Gas a whole step needs, from the vault's `x` down to its last callback.
pub fn need_tgas(n: &Node<Recipe>) -> u64 {
    let measure = 2 * VIEW_TGAS + ROUND_TGAS;
    let calls: u64 = n.action.calls.iter().map(|c| c.tgas() + ROUND_TGAS).sum();
    let price = if n.action.price.is_some() { VIEW_TGAS + ROUND_TGAS } else { 0 };
    ROUND_TGAS + 2 * measure + calls + price
}

#[near]
impl Brain {
    pub fn on_price(&self) {
        let (mut t, a): (Talk, PriceArgs) = callback();
        let amount = amount_result(0).unwrap_or_else(|| env::panic_str("price source failed"));
        require!(a.per > 0, "bad price source");
        let price = mul_div(amount, PRICE_SCALE, a.per);
        let ans = t.start(a.flow_id, a.node, Some(price), a.by_owner);
        t.save(ans).send();
    }

    pub fn on_measured(&self) {
        let (mut t, a): (Talk, StepArgs) = callback();
        let mut ctx = t.current(a.id);
        let (input, out) = t.tokens(&ctx);
        let mut i = 0;
        let mut next = || {
            i += 1;
            amount_result(i - 1)
        };
        let in_bal = input.map(|_| next());
        let out_bal = out.map(|_| next());
        let ans = if a.at == 1 {
            if in_bal == Some(None) || out_bal == Some(None) {
                // Couldn't even read a balance: nothing happened, give the input back.
                t.end(Err(ctx.amount.0), 0, false)
            } else {
                ctx.in_before = in_bal.flatten().map(U128);
                ctx.out_before = out_bal.flatten().map(U128);
                t.vs.busy = Some(ctx);
                t.run_call(0)
            }
        } else {
            let in_used = match (ctx.in_before, in_bal) {
                (Some(b), Some(Some(a))) => b.0.saturating_sub(a).min(ctx.amount.0),
                (Some(_), _) => ctx.amount.0,
                (None, _) => ctx.native_used.0,
            };
            let out_amount = match (ctx.out_before, out_bal) {
                (Some(b), Some(Some(a))) => a.saturating_sub(b.0),
                _ => 0,
            };
            let left = ctx.amount.0 - in_used;
            let collector = t.node(&ctx).input == flow::NO_INPUT;
            ctx.failed |= in_used == 0 && !collector;
            if ctx.failed {
                t.end(Err(left), out_amount, false)
            } else {
                t.end(Ok(out_amount), in_used, true)
            }
        };
        t.save(ans).send();
    }

    pub fn on_call(&self) {
        let (mut t, a): (Talk, StepArgs) = callback();
        let mut ctx = t.current(a.id);
        let calls = t.node(&ctx).action.calls;
        let i = a.at as usize;
        // ft_transfer_call "succeeds" even when the receiver refuses and refunds:
        // zero used = refused, and the rest of the recipe must not run.
        let ok = match (&calls[i], result(0)) {
            (_, None) => false,
            (Call::Ft { .. }, Some(raw)) => near_sdk::serde_json::from_slice::<U128>(&raw).map_or(false, |used| used.0 > 0),
            (Call::Fn { .. }, Some(_)) => true,
        };
        let ans = if ok {
            if matches!(calls[i], Call::Fn { deposit: Deposit::Input, .. }) {
                ctx.native_used = ctx.amount;
            }
            t.vs.busy = Some(ctx);
            if i + 1 < calls.len() { t.run_call(a.at + 1) } else { t.measure(false) }
        } else {
            ctx.failed = true;
            t.vs.busy = Some(ctx);
            t.measure(false)
        };
        t.save(ans).send();
    }
}

impl Talk {
    pub(crate) fn begin_step(&mut self, sender: &AccountId, flow_id: u32, node: u16) -> Answer {
        let by_owner = *sender == self.vs.owner;
        require!(by_owner || Some(sender) == self.vs.keeper.as_ref(), "owner or keeper only");
        self.idle();
        let n = self.flow(flow_id).nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
        // A step that runs out of gas halfway leaves the vault busy: refuse up front.
        let need = need_tgas(&n);
        require!(self.ctx.gas_left / 1_000_000_000_000 >= need, format!("attach at least {need} Tgas for this step"));
        if let Some(why) = self.untrusted(&n) {
            env::panic_str(&format!("untrusted: {why}"));
        }
        if matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
            let src = n.action.price.clone().unwrap_or_else(|| env::panic_str("price trigger without a price source"));
            let args = borsh::to_vec(&PriceArgs { flow_id, node, by_owner, per: src.per.0 }).unwrap();
            return Answer::default().call(&src.contract, &src.method, src.args.as_bytes(), 0, GAS_VIEW).then("on_price", args, GAS_BRAIN_CB);
        }
        self.start(flow_id, node, None, by_owner)
    }

    fn start(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool) -> Answer {
        self.idle();
        let now = env::block_timestamp();
        let f = self.flow_mut(flow_id);
        let amount = f.ledger.begin(&f.nodes, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let id = self.vs.next_step;
        self.vs.next_step += 1;
        self.vs.busy = Some(StepCtx {
            id,
            flow: flow_id,
            node,
            amount: U128(amount),
            started_ns: now,
            in_before: None,
            out_before: None,
            native_used: U128(0),
            failed: false,
        });
        self.measure(true)
    }

    pub(crate) fn unstick(&mut self) {
        let ctx = self.vs.busy.clone().unwrap_or_else(|| env::panic_str("nothing running"));
        require!(env::block_timestamp() >= ctx.started_ns + crate::STUCK_NS, "the step may still finish");
        self.vs.busy = None;
        let f = self.flow_mut(ctx.flow);
        let nodes = f.nodes.clone();
        let _ = f.ledger.finish(&nodes, ctx.node, Err(0));
        f.ledger.pause(ctx.node).ok();
    }

    /// The step in flight, which this callback must belong to.
    fn current(&self, id: u64) -> StepCtx {
        let ctx = self.vs.busy.clone().unwrap_or_else(|| env::panic_str("no step"));
        require!(ctx.id == id, "a callback from another step");
        ctx
    }

    fn node(&self, ctx: &StepCtx) -> Node<Recipe> {
        self.flow(ctx.flow).nodes[ctx.node as usize].clone()
    }

    fn tokens(&self, ctx: &StepCtx) -> (Option<AccountId>, Option<AccountId>) {
        let n = self.node(ctx);
        let input = if n.input == NEAR || n.input == flow::NO_INPUT { None } else { n.input.parse().ok() };
        (input, n.action.out.clone())
    }

    /// Read the vault's balances of the step's tokens, then `on_measured`.
    fn measure(&self, before: bool) -> Answer {
        let ctx = self.vs.busy.clone().unwrap();
        let (input, out) = self.tokens(&ctx);
        let mut a = Answer::default();
        for t in input.iter().chain(out.iter()) {
            a = a.call(t, "ft_balance_of", format!(r#"{{"account_id":"{}"}}"#, self.vault).as_bytes(), 0, GAS_VIEW);
        }
        // Nothing to measure (NEAR in, nothing out): the callback runs straight away.
        a.then("on_measured", borsh::to_vec(&StepArgs { id: ctx.id, at: before as u8 }).unwrap(), GAS_BRAIN_CB)
    }

    fn run_call(&mut self, i: u8) -> Answer {
        let mut ctx = self.vs.busy.clone().unwrap();
        let node = self.node(&ctx);
        let call = &node.action.calls[i as usize];
        // Not enough gas left to launch this call and come back: close the step
        // as failed (measuring what moved) rather than leave the vault stuck.
        let left = self.ctx.gas_left / 1_000_000_000_000;
        if left < call.tgas() + 2 * ROUND_TGAS + 2 * VIEW_TGAS + 20 {
            ctx.failed = true;
            self.vs.busy = Some(ctx);
            return self.measure(false);
        }
        let amount = ctx.amount.0;
        let min_out = mul_div(amount, node.action.min_out_per_in.map_or(0, |m| m.0), PRICE_SCALE);
        let scaled = node.action.scale_per_in.map_or(0, |m| mul_div(amount, m.0, PRICE_SCALE));
        let fill = |s: &str| {
            s.replace("{amount}", &amount.to_string())
                .replace("{scaled}", &scaled.to_string())
                .replace("{min_out}", &min_out.to_string())
                .replace("{owner}", self.vs.owner.as_str())
                .replace("{vault}", self.vault.as_str())
        };
        let args = borsh::to_vec(&StepArgs { id: ctx.id, at: i }).unwrap();
        let a = match call {
            Call::Ft { receiver, msg, tgas } => {
                let token: AccountId = node.input.parse().unwrap();
                let json = near_sdk::serde_json::json!({"receiver_id": receiver, "amount": amount.to_string(), "msg": fill(msg)}).to_string();
                Answer::default().call(&token, "ft_transfer_call", json.as_bytes(), 1, Gas::from_tgas(*tgas))
            }
            Call::Fn { contract, method, args, deposit, tgas } => {
                let attach = match deposit {
                    Deposit::None => 0,
                    Deposit::OneYocto => 1,
                    Deposit::Input => amount,
                    Deposit::Fixed { yocto } => yocto.0,
                };
                Answer::default().call(contract, method, fill(args).as_bytes(), attach, Gas::from_tgas(*tgas))
            }
        };
        a.then("on_call", args, GAS_BRAIN_CB)
    }

    /// Close the step. `result`: Ok(output) or Err(input that came back).
    fn end(&mut self, result: Result<u128, u128>, extra: u128, ok: bool) -> Answer {
        let ctx = self.vs.busy.take().unwrap();
        let node = self.node(&ctx);
        let min = mul_div(extra, node.action.min_out_per_in.map_or(0, |m| m.0), PRICE_SCALE);
        let promises_output = node.action.out.is_some() && node.input != flow::NO_INPUT;
        let f = self.flow_mut(ctx.flow);
        let nodes = f.nodes.clone();
        f.ledger.finish(&nodes, ctx.node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let mut paused = false;
        if let Ok(out) = result {
            if out < min || (promises_output && out == 0) {
                f.ledger.pause(ctx.node).ok();
                paused = true;
            }
        }
        let (r, v) = match result {
            Ok(v) | Err(v) => (if ok { "out" } else { "returned" }, v),
        };
        let data = format!(
            r#""flow":{},"node":{},"amount":"{}","{r}":"{v}","{}":"{extra}","paused":{paused}"#,
            ctx.flow,
            ctx.node,
            ctx.amount.0,
            if ok { "used" } else { "stray_out" }
        );
        self.emit(if ok { "step" } else { "step_failed" }, &data);
        Answer::default().ret(&format!("{{{data}}}"))
    }
}
