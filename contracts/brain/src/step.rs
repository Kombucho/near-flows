//! One step of a flow. Balances and prices are views,
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
use crate::{amount_result, mul_div, read_tgas, result, Call, Deposit, Next, Read, Recipe, Reply, Talk, NEAR, OWN_TGAS, PRICE_SCALE, RELAY_TGAS};

/// The callback deciding on the balances a step read (on_measured): it only
/// answers, burning 2.5-3.1 Tgas; 8 is a 2.5x margin.
const GAS_MEASURE: Gas = Gas::from_tgas(8);
/// The brain's callback after a recipe call that has another call after it: it
/// only answers the next call (burns 3.0-4.0 Tgas, as on_call after the last).
/// If the call failed, its balance read doesn't fit here and moves to the
/// vault's next round (`read`), which the rest of the recipe's gas pays for.
const ON_CALL_MID_TGAS: u64 = 8;

/// Tgas to read the step's two balances and decide (2 views, on_measured).
pub fn measure_tgas() -> u64 {
    read_tgas(2, GAS_MEASURE.as_tgas())
}

/// The brain's callback after the recipe's last call: reads the balances in place
/// (27) plus its own work (6). Was 45 (then 30) with 20 Tgas reads.
pub fn on_last_call_tgas() -> u64 {
    measure_tgas() + OWN_TGAS
}

fn on_call_tgas(last: bool) -> u64 {
    if last { on_last_call_tgas() } else { ON_CALL_MID_TGAS }
}

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
    /// The keeper (not the owner) runs this step: it is paid its fee if the step works.
    pub by_keeper: bool,
}

/// Gas the vault's rounds need after the balances are read: each call, the
/// brain's callback after it and the vault's `cb`, then the last `cb`.
pub fn need_tgas(n: &Node<Recipe>) -> u64 {
    let k = n.action.calls.len();
    n.action.calls.iter().enumerate().map(|(i, c)| c.tgas() + on_call_tgas(i + 1 == k) + RELAY_TGAS).sum::<u64>() + RELAY_TGAS
}

/// Gas a step of `n` needs after the relay's first round: its first read (the
/// trigger's price, else the oracle feeds (`feed_views` of them), else the
/// balances) runs inside the first round when it fits beside the brain's own
/// work, or in a round of its own; then the recipe. `save_flow` checks this.
pub fn worst_tgas(n: &Node<Recipe>, feed_views: usize) -> u64 {
    let (price_cb, _) = price_gas(n, feed_views);
    let first = if price_cb > 0 { read_tgas(1, price_cb) } else if feed_views > 0 { read_tgas(feed_views, feeds_cb()) } else { measure_tgas() };
    let moved = if first + OWN_TGAS + crate::FINISH_TGAS <= crate::HANDLE_TGAS { 0 } else { first + OWN_TGAS + RELAY_TGAS };
    moved + need_tgas(n)
}

/// on_step_prices: checks the floor, then reads the balances in place.
fn feeds_cb() -> u64 {
    OWN_TGAS + measure_tgas()
}

/// The price trigger's callback gas (0: no trigger), and the feeds' read after it.
fn price_gas(n: &Node<Recipe>, feed_views: usize) -> (u64, u64) {
    if !matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
        return (0, 0);
    }
    let after = if n.action.market.is_some() { read_tgas(feed_views, feeds_cb()) } else { measure_tgas() };
    (OWN_TGAS + after, after)
}

impl Talk {
    /// Run node `n` (stored, or revealed by the caller and already checked).
    pub(crate) fn begin_step(&mut self, sender: &AccountId, flow_id: u32, node: u16, n: Node<Recipe>, hidden: bool) -> Reply {
        let by_owner = *sender == self.vs.owner;
        require!(by_owner || Some(sender) == self.vs.keeper.as_ref(), "owner or keeper only");
        self.idle();
        // No free work: a keeper run the vault can't pay for is refused before anything moves.
        self.require_keeper_paid(by_owner);
        require!(n.action.remote.is_none(), "a remote node runs with step_remote");
        // A step that runs out of gas halfway leaves the vault busy: refuse up front.
        // (A read moved to its own round checks its share too: `read`.)
        let need = need_tgas(&n);
        require!(self.ctx.gas_left / 1_000_000_000_000 >= need, format!("attach at least {} Tgas for this step", need + crate::HANDLE_TGAS + crate::X_TGAS));
        self.need_after = need;
        // Trust can be withdrawn after a flow was saved: check again before every step.
        self.check_node(&n);
        // A fixed deposit is the vault's own NEAR, never a flow's or the reserve.
        let fixed = fixed_deposits(&n);
        require!(fixed <= self.free_near(), format!("refused: the recipe attaches {fixed} yoctoNEAR and the vault's free NEAR is {}: add NEAR", self.free_near()));
        let revealed = hidden.then(|| n.clone());
        if matches!(n.when, flow::Trigger::PriceBelow { .. } | flow::Trigger::PriceAbove { .. }) {
            let src = n.action.price.clone().unwrap_or_else(|| env::panic_str("price trigger without a price source"));
            // Its callback may read the market, then the balances, in place.
            let (cb, _) = price_gas(&n, self.feed_count(&n));
            let read = Read::Price { flow_id, node, by_owner, per: src.per.0, revealed };
            return self.read(vec![(src.contract, src.method, src.args)], read, Gas::from_tgas(cb));
        }
        self.start(flow_id, node, None, by_owner, revealed)
    }

    /// How many oracle views a step of `n` makes: per feed its price and maybe a
    /// backup. A feed not set up yet counts as two (the most it can be).
    pub(crate) fn feed_count(&self, n: &Node<Recipe>) -> usize {
        let Some(m) = n.action.market.as_ref() else { return 0 };
        let feeds = self.vs.oracle.as_ref().map(|c| c.feeds.clone()).unwrap_or_default();
        [&m.sell, &m.buy].iter().map(|name| feeds.iter().find(|f| f.name == **name).map_or(2, |f| 1 + f.backup.is_some() as usize)).sum()
    }

    /// Refuse a node whose step can never fit in a mainnet transaction (300 Tgas):
    /// the relay's `x`, the brain's first round, then `worst_tgas`.
    pub(crate) fn check_gas(&self, i: usize, n: &Node<Recipe>) {
        if n.action.remote.is_some() {
            return; // a fixed path: step_remote checks its own need
        }
        let total = crate::X_TGAS + crate::HANDLE_TGAS + worst_tgas(n, self.feed_count(n));
        require!(total <= crate::MAINNET_TGAS, format!("node {i} needs {total} Tgas per step, more than the {} a mainnet transaction may carry: give its calls less gas", crate::MAINNET_TGAS));
    }

    fn node_of(&self, flow_id: u32, node: u16, revealed: &Option<Node<Recipe>>) -> Node<Recipe> {
        revealed.clone().unwrap_or_else(|| self.flow(flow_id).nodes[node as usize].clone())
    }

    pub(crate) fn on_price(&mut self, flow_id: u32, node: u16, by_owner: bool, per: u128, revealed: Option<Node<Recipe>>) -> Reply {
        let amount = amount_result(0).unwrap_or_else(|| env::panic_str("price source failed"));
        require!(per > 0, "bad price source");
        self.start(flow_id, node, Some(mul_div(amount, PRICE_SCALE, per)), by_owner, revealed)
    }

    /// Start the step; a recipe with a market floor has the brain read the oracle first.
    fn start(&mut self, flow_id: u32, node: u16, price: Option<u128>, by_owner: bool, revealed: Option<Node<Recipe>>) -> Reply {
        let n = self.node_of(flow_id, node, &revealed);
        self.need_after = need_tgas(&n);
        match n.action.market {
            None => self.start_now(flow_id, node, price, by_owner, revealed, None),
            Some(m) => {
                let views = self.feed_views(&[m.sell.clone(), m.buy.clone()]);
                let read = Read::StepPrices { flow_id, node, price, by_owner, revealed };
                self.read(views, read, Gas::from_tgas(feeds_cb()))
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
        self.need_after = need_tgas(&n);
        self.charge(&n.input, amount);
        // Its tokens are read on close; what an out: null node did isn't measured (close.rs).
        self.note_node(&n);
        self.vs.elsewhere |= n.action.out.is_none();
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
            by_keeper: !by_owner,
        });
        self.measure(true)
    }

    pub(crate) fn unstick(&mut self) {
        let ctx = self.vs.busy.clone().unwrap_or_else(|| env::panic_str("nothing running"));
        require!(env::block_timestamp() >= ctx.started_ns + crate::STUCK_NS, "the step may still finish");
        self.vs.busy = None;
        // Out of gas halfway: what its calls did is unknown (close.rs).
        self.vs.elsewhere = true;
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
        self.read(views, Read::Measured { id: ctx.id, before }, GAS_MEASURE)
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
        let on_call = on_call_tgas(i as usize + 1 == node.action.calls.len());
        if self.ctx.gas_left / 1_000_000_000_000 < call.tgas() + on_call + 2 * RELAY_TGAS {
            ctx.failed = true;
            self.vs.busy = Some(ctx);
            return self.measure(false);
        }
        // The fixed deposit was checked when the step began; the free NEAR may have left since.
        if let Call::Fn { deposit: Deposit::Fixed { yocto }, .. } = call {
            if yocto.0 > self.free_near() {
                ctx.failed = true;
                self.vs.busy = Some(ctx);
                return self.measure(false);
            }
            self.spend(yocto.0);
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
        let next = near_sdk::borsh::to_vec(&Next::Call { id: ctx.id, i, tgas: on_call }).unwrap();
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
        a.then("on_vault", next, Gas::from_tgas(on_call)).into()
    }

    /// Close the step. `result`: Ok(output) or Err(input that came back).
    fn end(&mut self, result: Result<u128, u128>, extra: u128, ok: bool) -> Reply {
        let ctx = self.vs.busy.take().unwrap();
        let node = self.node(&ctx);
        let min = mul_div(extra, min_per_in(&node.action, &ctx), PRICE_SCALE);
        // A collector finding nothing yet is normal; only spenders must get something back.
        let promises_output = node.action.out.is_some() && node.input != flow::NO_INPUT;
        // Input spent for nothing measured back (a deposit the receiver keeps), or lost
        // on a failure: it may sit inside another contract now (close.rs).
        match result {
            Ok(out) if extra > 0 && out == 0 => self.vs.elsewhere = true,
            Err(left) if left < ctx.amount.0 => self.vs.elsewhere = true,
            _ => {}
        }
        let f = self.flow_mut(ctx.flow);
        let done = match result {
            // A partial fill: what the step didn't use stays the node's.
            Ok(out) => f.ledger.finish_node_partial(&node, ctx.node, out, ctx.amount.0.saturating_sub(extra)),
            Err(_) => f.ledger.finish_node(&node, ctx.node, result),
        };
        done.unwrap_or_else(|e| env::panic_str(&format!("{e:?}")));
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
        let ans = Answer::default().ret(&format!("{{{data}}}"));
        // Fees only for a step that did its job: the protocol's share of what it used,
        // the keeper's price if it spent input or collected something (a collector
        // that found nothing did no work worth paying for).
        let produced = extra > 0 || matches!(result, Ok(out) if out > 0);
        if ok { self.step_fees(ans, &node.input, extra, ctx.by_keeper && produced).into() } else { ans.into() }
    }
}

/// NEAR the recipe attaches of its own (fixed deposits), outside the node's input.
fn fixed_deposits(n: &Node<Recipe>) -> u128 {
    n.action.calls.iter().map(|c| match c {
        Call::Fn { deposit: Deposit::Fixed { yocto }, .. } => yocto.0,
        _ => 0,
    }).sum()
}

/// The least output per unit of input (1e24): the owner's floor or the market's, whichever is higher.
fn min_per_in(r: &Recipe, ctx: &StepCtx) -> u128 {
    r.min_out_per_in.map_or(0, |m| m.0).max(ctx.market_per_in.map_or(0, |m| m.0))
}
