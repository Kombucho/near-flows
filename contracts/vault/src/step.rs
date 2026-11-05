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

use crate::{Call, Deposit, Vault, VaultExt, NEAR, PRICE_SCALE};

const GAS_VIEW: Gas = Gas::from_tgas(5);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);

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
}

#[near]
impl Vault {
    /// Run node `node` of flow `flow_id`, if its trigger has fired. Owner or keeper.
    pub fn step(&mut self, flow_id: u32, node: u16) -> Promise {
        let by_owner = env::predecessor_account_id() == self.owner;
        require!(by_owner || Some(env::predecessor_account_id()) == self.keeper, "owner or keeper only");
        require!(self.busy.is_none(), "a step is running");
        let n = self.flow(flow_id).nodes.get(node as usize).unwrap_or_else(|| env::panic_str("no such node")).clone();
        if matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
            let src = n.action.price.clone().unwrap_or_else(|| env::panic_str("price trigger without a price source"));
            return Promise::new(src.contract)
                .function_call(src.method, src.args.into_bytes(), NearToken::from_yoctonear(0), GAS_VIEW)
                .then(self.cb().on_price(flow_id, node, by_owner, src.per));
        }
        self.start(flow_id, node, None, by_owner)
    }

    #[private]
    pub fn on_price(&mut self, flow_id: u32, node: u16, by_owner: bool, per: U128) -> Promise {
        let raw = result(0).unwrap_or_else(|| env::panic_str("price source failed"));
        let amount: U128 = near_sdk::serde_json::from_slice(&raw).unwrap_or_else(|_| env::panic_str("price source returned no amount"));
        require!(per.0 > 0, "bad price source");
        let price = crate::mul_div(amount.0, PRICE_SCALE, per.0);
        self.start(flow_id, node, Some(price), by_owner)
    }

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
        if ctx.failed || in_used == 0 {
            self.end(Err(left), out_amount, false)
        } else {
            self.end(Ok(out_amount), in_used, true)
        }
    }

    #[private]
    pub fn on_call(&mut self, i: u8) -> Promise {
        let mut ctx = self.busy.clone().unwrap_or_else(|| env::panic_str("no step"));
        let ok = result(0).is_some();
        let calls = self.recipe_calls(&ctx);
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

    fn start(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool) -> Promise {
        require!(self.busy.is_none(), "a step is running");
        let now = env::block_timestamp();
        let f = self.flow_mut(flow_id);
        let amount = f.ledger.begin(&f.nodes, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.busy = Some(StepCtx {
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

    /// (input token if it's a token, output token if any)
    fn tokens(&self, ctx: &StepCtx) -> (Option<AccountId>, Option<AccountId>) {
        let n = &self.flow(ctx.flow).nodes[ctx.node as usize];
        let input = if n.input == NEAR { None } else { n.input.parse().ok() };
        (input, n.action.out.clone())
    }

    fn recipe_calls(&self, ctx: &StepCtx) -> Vec<Call> {
        self.flow(ctx.flow).nodes[ctx.node as usize].action.calls.clone()
    }

    /// Read the vault's balances of the step's tokens, then continue in `on_measured`.
    fn measure(&mut self, before: bool) -> Promise {
        let ctx = self.busy.clone().unwrap();
        let (input, out) = self.tokens(&ctx);
        let me = env::current_account_id();
        let views: Vec<Promise> =
            input.into_iter().chain(out).map(|t| ext_ft_core::ext(t).with_static_gas(GAS_VIEW).ft_balance_of(me.clone())).collect();
        let mut it = views.into_iter();
        let joint = match it.next() {
            Some(first) => it.fold(first, |acc, p| acc.and(p)),
            // Nothing to measure (NEAR in, nothing out): a no-op view keeps the chain uniform.
            None => Promise::new(me.clone()).function_call("get_owner".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), GAS_VIEW),
        };
        joint.then(self.cb().on_measured(before))
    }

    fn run_call(&mut self, i: u8) -> Promise {
        let ctx = self.busy.clone().unwrap();
        let node = self.flow(ctx.flow).nodes[ctx.node as usize].clone();
        let amount = ctx.amount.0;
        let min_out = node.action.min_out_per_in.map_or(0, |m| crate::mul_div(amount, m.0, PRICE_SCALE));
        let fill = |s: &str| {
            s.replace("{amount}", &amount.to_string())
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
        let min = {
            let n = &self.flow(ctx.flow).nodes[ctx.node as usize];
            n.action.min_out_per_in.map_or(0, |m| crate::mul_div(extra, m.0, PRICE_SCALE))
        };
        let f = self.flow_mut(ctx.flow);
        f.ledger.finish(&f.nodes, ctx.node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        let mut paused = false;
        if let Ok(out) = result {
            if out < min {
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
        Promise::new(env::current_account_id()).function_call("get_owner".to_string(), b"{}".to_vec(), NearToken::from_yoctonear(0), GAS_VIEW)
    }
}
