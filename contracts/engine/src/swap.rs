//! Swaps that walk across bins.
//!
//! Two phases, so a failed trade never leaves half-changed state:
//!   1. plan:   read-only, work out every bin fill
//!   2. commit: check the trader's minimum, then apply all fills

use math::acc::accrue;
use math::fees::{fee_on, fee_on_net, split_fee};
use math::in_bin::{buy_token, sell_token};
use math::wide::mul_div_up;
use math::PRICE_SCALE;

use crate::{Book, Error};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Buy,  // pay quote, receive token (walks UP through bins)
    Sell, // pay token, receive quote (walks DOWN through bins)
}

#[derive(Debug, PartialEq, Eq)]
pub struct SwapResult {
    pub used: u128,   // amount_in actually spent (fees included)
    pub out: u128,    // what the trader receives
    pub fee: u128,    // total fee paid
    pub refund: u128, // unspent input, returned to the trader
}

/// One bin's part of a trade.
struct Step {
    bin: i32,
    net: u128, // input paid for this bin's output (fee excluded)
    fee: u128, // fee charged in this bin
    out: u128, // output taken from the bin
}

/// How a buy's fill in one bin divides between the take-profit layer
/// (filled first) and the two-way LP layer.
struct BuySplit {
    out_tp: u128,
    out_lp: u128,
    net_tp: u128,
    net_lp: u128,
    fee_tp: u128,
    fee_lp: u128,
}

fn split_buy(s: &Step, tp_token: u128) -> BuySplit {
    let out_tp = s.out.min(tp_token);
    // Pro-rata by tokens taken, rounded down for the tp side: the LP layer gets
    // the odd unit, so tp owners are never credited more than was paid.
    let net_tp = math::wide::mul_div_down(s.net, out_tp, s.out);
    let fee_tp = math::wide::mul_div_down(s.fee, out_tp, s.out);
    BuySplit { out_tp, out_lp: s.out - out_tp, net_tp, net_lp: s.net - net_tp, fee_tp, fee_lp: s.fee - fee_tp }
}

impl Book {
    pub fn swap(&mut self, side: Side, amount_in: u128, min_out: u128) -> Result<SwapResult, Error> {
        self.swap_within(side, amount_in, min_out, None)
    }

    /// Like `swap`, but never fills beyond bin `limit` (above it for buys,
    /// below it for sells). The protocol's own buyback uses this so it can never
    /// be tricked into paying a far-away price for a dust ask.
    pub fn swap_within(
        &mut self, side: Side, amount_in: u128, min_out: u128, limit: Option<i32>,
    ) -> Result<SwapResult, Error> {
        if amount_in == 0 {
            return Err(Error::ZeroAmount);
        }
        let (steps, left) = self.plan(side, amount_in, limit);
        let out: u128 = steps.iter().map(|s| s.out).sum();
        if out == 0 {
            return Err(Error::NoLiquidity);
        }
        if out < min_out {
            return Err(Error::Slippage { out, min_out });
        }
        let fee = steps.iter().map(|s| s.fee).sum();
        for s in &steps {
            self.apply(side, s);
        }
        self.active = steps.last().map(|s| s.bin).unwrap_or(self.active);
        Ok(SwapResult { used: amount_in - left, out, fee, refund: left })
    }

    /// Bins that can fill this side, in walking order, read lazily: the walk
    /// stops at the first bin that completes the order, never touching the rest.
    /// `Box<dyn Iterator>` lets the two directions share one return type.
    fn walk(&self, side: Side) -> Box<dyn Iterator<Item = (i32, u128)> + '_> {
        match side {
            // Buyers can take both layers: take-profit orders and LP tokens.
            Side::Buy => Box::new(self.bins.range(self.active..).filter(|(_, b)| b.token + b.tp_token > 0).map(|(k, b)| (*k, b.token + b.tp_token))),
            // Sellers only ever see the LP layer's quote. Take-profit proceeds are invisible.
            Side::Sell => Box::new(self.bins.range(..=self.active).rev().filter(|(_, b)| b.quote > 0).map(|(k, b)| (*k, b.quote))),
        }
    }

    /// Read-only: how the trade would fill. Returns the steps and unspent input.
    fn plan(&self, side: Side, amount_in: u128, limit: Option<i32>) -> (Vec<Step>, u128) {
        let fee_bps = self.cfg.fee_bps;
        let mut left = amount_in;
        let mut steps = Vec::new();
        for (bin, reserve) in self.walk(side) {
            let past_limit = match (side, limit) {
                (Side::Buy, Some(l)) => bin > l,
                (Side::Sell, Some(l)) => bin < l,
                _ => false,
            };
            if past_limit {
                break; // the rest of the order is refunded
            }
            let price = self.price(bin);
            // Net input needed to empty this bin, then the fee on top of it.
            let need = match side {
                Side::Buy => mul_div_up(reserve, price, PRICE_SCALE),
                Side::Sell => mul_div_up(reserve, PRICE_SCALE, price),
            };
            let need_gross = need + fee_on_net(need, fee_bps);
            if left >= need_gross {
                steps.push(Step { bin, net: need, fee: need_gross - need, out: reserve });
                left -= need_gross;
                if left == 0 { break; }
                continue;
            }
            // Partial fill: this bin absorbs the rest of the order.
            let fee = fee_on(left, fee_bps);
            let fill = match side {
                Side::Buy => buy_token(reserve, price, left - fee),
                Side::Sell => sell_token(reserve, price, left - fee),
            };
            if fill.out > 0 {
                steps.push(Step { bin, net: fill.used_in, fee, out: fill.out });
                left -= fill.used_in + fee;
            }
            break;
        }
        (steps, left)
    }

    /// Apply one planned step: move reserves and hand out the fee.
    fn apply(&mut self, side: Side, s: &Step) {
        match side {
            Side::Buy => self.apply_buy(s),
            Side::Sell => self.apply_sell(s),
        }
    }

    fn apply_buy(&mut self, s: &Step) {
        let tp_token = self.bins[&s.bin].tp_token;
        let x = split_buy(s, tp_token);
        if x.out_tp > 0 {
            // Take-profit owners get the price paid plus the LP slice of their fee.
            let f = split_fee(x.fee_tp, self.cfg.shares);
            self.pots.pot_quote += f.pot;
            self.pots.creator_quote += f.creator;
            self.tp_fill(s.bin, x.out_tp, x.net_tp + f.lps);
        }
        let mut f = split_fee(x.fee_lp, self.cfg.shares);
        let b = self.bins.get_mut(&s.bin).expect("planned bin exists");
        if b.shares == 0 {
            // No LPs here (only rounding dust): their slice goes to the pot.
            f.pot += f.lps;
            f.lps = 0;
        }
        b.token -= x.out_lp;
        b.quote += x.net_lp;
        b.acc_quote = accrue(b.acc_quote, f.lps, b.shares);
        self.pots.pot_quote += f.pot;
        self.pots.creator_quote += f.creator;
    }

    fn apply_sell(&mut self, s: &Step) {
        let mut f = split_fee(s.fee, self.cfg.shares);
        let b = self.bins.get_mut(&s.bin).expect("planned bin exists");
        if b.shares == 0 {
            f.pot += f.lps;
            f.lps = 0;
        }
        b.quote -= s.out;
        b.token += s.net;
        b.acc_token = accrue(b.acc_token, f.lps, b.shares);
        self.pots.pot_token += f.pot;
        self.pots.creator_token += f.creator;
    }
}
