//! One step of a flow, as in vault/src/step.rs. Balances and prices are views,
//! so the brain reads them ITSELF (its own promises, `on_read`); only the
//! recipe's calls go through the vault, because only they must come from it:
//!
//!   vault x -> brain: [price view] -> [market floor] -> balances before -> answer: call 1
//!   vault cb runs call 1 -> brain on_vault -> answer: call 2 ...
//!   ... last call -> brain on_vault: balances after -> answer: done
//!
//! Each callback carries the step's id: a late callback from a step that was
//! cleared (`unstick`) can't land in the next step.

use near_sdk::json_types::U128;
use near_sdk::{env, near, require, AccountId, Gas};

use flow::Node;

use crate::wire::Answer;
use crate::{amount_result, mul_div, result, Call, Deposit, Next, Read, Recipe, Reply, Talk, GAS_READ, NEAR, PRICE_SCALE};

/// The brain's callback after a recipe call: it may read two balances and decide again.
pub(crate) const GAS_ON_CALL: Gas = Gas::from_tgas(45);
/// The vault's `cb` (relay GAS_CB) and receipt costs, per round.
const RELAY_TGAS: u64 = 10;

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
    /// A hidden flow's node, as revealed to run this step.
    pub revealed: Option<Node<Recipe>>,
    /// The market floor read from the oracle for this step (per unit of input, 1e24).
    pub market_per_in: Option<U128>,
}

/// Gas the vault's rounds need after the brain's first answer.
pub fn need_tgas(n: &Node<Recipe>) -> u64 {
    n.action.calls.iter().map(|c| c.tgas() + GAS_ON_CALL.as_tgas() + RELAY_TGAS).sum::<u64>() + RELAY_TGAS
}

impl Talk {
    /// Run node `n` (stored, or revealed by the caller and already checked).
    pub(crate) fn begin_step(&mut self, sender: &AccountId, flow_id: u32, node: u16, n: Node<Recipe>, hidden: bool) -> Reply {
        let by_owner = *sender == self.vs.owner;
        require!(by_owner || Some(sender) == self.vs.keeper.as_ref(), "owner or keeper only");
        self.idle();
        require!(n.action.remote.is_none(), "a remote node runs with step_remote");
        // A step that runs out of gas halfway leaves the vault busy: refuse up front.
        let need = need_tgas(&n);
        require!(self.ctx.gas_left / 1_000_000_000_000 >= need, format!("attach at least {} Tgas for this step", need + crate::HANDLE_TGAS));
        // Trust can be withdrawn after a flow was saved: check again before every step.
        self.check_node(&n);
        let revealed = hidden.then(|| n.clone());
        if matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
            let src = n.action.price.clone().unwrap_or_else(|| env::panic_str("price trigger without a price source"));
            // Its callback may read the market and then the balances: three reads' worth.
            let read = Read::Price { flow_id, node, by_owner, per: src.per.0, revealed };
            return self.read(vec![(src.contract, src.method, src.args)], read, Gas::from_tgas(3 * GAS_READ.as_tgas()));
        }
        self.start(flow_id, node, None, by_owner, revealed)
    }

    pub(crate) fn on_price(&mut self, flow_id: u32, node: u16, by_owner: bool, per: u128, revealed: Option<Node<Recipe>>) -> Reply {
        let amount = amount_result(0).unwrap_or_else(|| env::panic_str("price source failed"));
        require!(per > 0, "bad price source");
        self.start(flow_id, node, Some(mul_div(amount, PRICE_SCALE, per)), by_owner, revealed)
    }

    /// Start the step; a recipe with a market floor has the brain read the oracle first.
    fn start(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>) -> Reply {
        let market = revealed.as_ref().unwrap_or_else(|| &self.flow(flow_id).nodes[node as usize]).action.market.clone();
        match market {
            None => self.start_now(flow_id, node, price, by_owner, revealed, None),
            Some(m) => {
                let views = self.feed_views(&[m.sell.clone(), m.buy.clone()]);
                let read = Read::StepPrices { flow_id, node, price, by_owner, revealed };
                self.read(views, read, Gas::from_tgas(2 * GAS_READ.as_tgas()))
            }
        }
    }

    /// The oracle answered for a recipe with a market floor: check the prices, then start.
    pub(crate) fn on_step_prices(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>) -> Reply {
        let m = revealed.as_ref().unwrap_or_else(|| &self.flow(flow_id).nodes[node as usize]).action.market.clone().unwrap();
        let p = self.prices(&[m.sell.clone(), m.buy.clone()], 0);
        let per_in = crate::oracle::market_per_in(&m, p[0], p[1], PRICE_SCALE);
        self.emit("market_floor", &format!(r#""flow":{flow_id},"node":{node},"sell":"{}","buy":"{}","min_out_per_in":"{per_in}""#, m.sell, m.buy));
        self.start_now(flow_id, node, price, by_owner, revealed, Some(per_in))
    }

    fn start_now(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>, market_per_in: Option<u128>) -> Reply {
        self.idle();
        let now = env::block_timestamp();
        let f = self.flow_mut(flow_id);
        let n = revealed.clone().unwrap_or_else(|| f.nodes[node as usize].clone());
        let amount = f.ledger.begin_node(&n, node, now, price, by_owner).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
        self.charge(&n.input, amount);
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
            revealed,
            market_per_in: market_per_in.map(U128),
        });
        self.measure(true)
    }

    pub(crate) fn unstick(&mut self) {
        let ctx = self.vs.busy.clone().unwrap_or_else(|| env::panic_str("nothing running"));
        require!(env::block_timestamp() >= ctx.started_ns + crate::STUCK_NS, "the step may still finish");
        self.vs.busy = None;
        let n = self.node(&ctx);
        let f = self.flow_mut(ctx.flow);
        let _ = f.ledger.finish_node(&n, ctx.node, Err(0));
        f.ledger.pause(ctx.node).ok();
    }

    /// The step in flight, which this callback must belong to.
    fn current(&self, id: u64) -> StepCtx {
        let ctx = self.vs.busy.clone().unwrap_or_else(|| env::panic_str("no step"));
        require!(ctx.id == id, "a callback from another step");
        ctx
    }

    fn node(&self, ctx: &StepCtx) -> Node<Recipe> {
        ctx.revealed.clone().unwrap_or_else(|| self.flow(ctx.flow).nodes[ctx.node as usize].clone())
    }

    fn tokens(&self, ctx: &StepCtx) -> (Option<AccountId>, Option<AccountId>) {
        let n = self.node(ctx);
        let input = if n.input == NEAR || n.input == flow::NO_INPUT { None } else { n.input.parse().ok() };
        (input, n.action.out.clone())
    }

    /// The brain reads the vault's balances of the step's tokens, then `on_measured`.
    /// Nothing to read (NEAR in, nothing out): decide right away.
    fn measure(&mut self, before: bool) -> Reply {
        let ctx = self.vs.busy.clone().unwrap();
        let (input, out) = self.tokens(&ctx);
        if input.is_none() && out.is_none() {
            return self.measured(ctx, before, None, None);
        }
        let views = input.iter().chain(out.iter()).map(|t| self.balance_view(t)).collect();
        self.read(views, Read::Measured { id: ctx.id, before }, GAS_READ)
    }

    pub(crate) fn on_measured(&mut self, id: u64, before: bool) -> Reply {
        let ctx = self.current(id);
        let (input, out) = self.tokens(&ctx);
        let mut i = 0;
        let mut next = || {
            i += 1;
            amount_result(i - 1)
        };
        let in_bal = input.map(|_| next());
        let out_bal = out.map(|_| next());
        self.measured(ctx, before, in_bal, out_bal)
    }

    /// `in_bal` / `out_bal`: None = no such token; Some(None) = the read failed.
    fn measured(&mut self, mut ctx: StepCtx, before: bool, in_bal: Option<Option<u128>>, out_bal: Option<Option<u128>>) -> Reply {
        if before {
            if in_bal == Some(None) || out_bal == Some(None) {
                // Couldn't even read a balance: nothing happened, give the input back.
                return self.end(Err(ctx.amount.0), 0, false);
            }
            ctx.in_before = in_bal.flatten().map(U128);
            ctx.out_before = out_bal.flatten().map(U128);
            self.vs.busy = Some(ctx);
            return self.run_call(0);
        }
        let in_used = match (ctx.in_before, in_bal) {
            (Some(b), Some(Some(a))) => b.0.saturating_sub(a).min(ctx.amount.0),
            (Some(_), _) => ctx.amount.0, // unknown: assume spent (the rest shows up as free)
            (None, _) => ctx.native_used.0,
        };
        let out_amount = match (ctx.out_before, out_bal) {
            (Some(b), Some(Some(a))) => a.saturating_sub(b.0),
            _ => 0,
        };
        let left = ctx.amount.0 - in_used;
        // A call can "succeed" without doing anything (a refused ft_transfer_call
        // just refunds): nothing spent = failed, unless it's a collector.
        let collector = self.node(&ctx).input == flow::NO_INPUT;
        if ctx.failed || (in_used == 0 && !collector) {
            self.end(Err(left), out_amount, false)
        } else {
            self.end(Ok(out_amount), in_used, true)
        }
    }

    /// The vault's recipe call `i` came back.
    pub(crate) fn on_call(&mut self, id: u64, i: u8) -> Reply {
        let mut ctx = self.current(id);
        let calls = self.node(&ctx).action.calls;
        let c = i as usize;
        // ft_transfer_call "succeeds" even when the receiver refuses and refunds:
        // zero used = refused, and the rest of the recipe must not run.
        let ok = match (&calls[c], result(0, 1024)) {
            (_, None) => false,
            (Call::Ft { .. }, Some(raw)) => near_sdk::serde_json::from_slice::<U128>(&raw).map_or(false, |used| used.0 > 0),
            (Call::Fn { .. }, Some(_)) => true,
        };
        if ok {
            if matches!(calls[c], Call::Fn { deposit: Deposit::Input, .. }) {
                ctx.native_used = ctx.amount;
            }
            self.vs.busy = Some(ctx);
            if c + 1 < calls.len() {
                return self.run_call(i + 1);
            }
        } else {
            ctx.failed = true;
            self.vs.busy = Some(ctx);
        }
        self.measure(false)
    }

    /// Answer: the vault runs recipe call `i`, then `on_vault` Call.
    fn run_call(&mut self, i: u8) -> Reply {
        let mut ctx = self.vs.busy.clone().unwrap();
        let node = self.node(&ctx);
        let call = &node.action.calls[i as usize];
        // Not enough gas left in the vault's next round to launch this call and
        // come back: close the step as failed rather than leave the vault stuck.
        if self.ctx.gas_left / 1_000_000_000_000 < call.tgas() + GAS_ON_CALL.as_tgas() + 2 * RELAY_TGAS {
            ctx.failed = true;
            self.vs.busy = Some(ctx);
            return self.measure(false);
        }
        let amount = ctx.amount.0;
        let min_out = mul_div(amount, min_per_in(&node.action, &ctx), PRICE_SCALE);
        let scaled = node.action.scale_per_in.map_or(0, |m| mul_div(amount, m.0, PRICE_SCALE));
        let fill = |s: &str| {
            s.replace("{amount}", &amount.to_string())
                .replace("{scaled}", &scaled.to_string())
                .replace("{min_out}", &min_out.to_string())
                .replace("{owner}", self.vs.owner.as_str())
                .replace("{vault}", self.vault.as_str())
        };
        let next = near_sdk::borsh::to_vec(&Next::Call { id: ctx.id, i }).unwrap();
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
        a.then("on_vault", next, GAS_ON_CALL).into()
    }

    /// Close the step. `result`: Ok(output) or Err(input that came back).
    fn end(&mut self, result: Result<u128, u128>, extra: u128, ok: bool) -> Reply {
        let ctx = self.vs.busy.take().unwrap();
        let node = self.node(&ctx);
        let min = mul_div(extra, min_per_in(&node.action, &ctx), PRICE_SCALE);
        // A collector finding nothing yet is normal; only spenders must get something back.
        let promises_output = node.action.out.is_some() && node.input != flow::NO_INPUT;
        let f = self.flow_mut(ctx.flow);
        f.ledger.finish_node(&node, ctx.node, result).unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
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
        Answer::default().ret(&format!("{{{data}}}")).into()
    }
}

/// The least output per unit of input (1e24): the owner's floor or the market's, whichever is higher.
fn min_per_in(r: &Recipe, ctx: &StepCtx) -> u128 {
    r.min_out_per_in.map_or(0, |m| m.0).max(ctx.market_per_in.map_or(0, |m| m.0))
}
