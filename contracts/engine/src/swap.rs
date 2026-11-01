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
    net: u128, // input that went into the bin's reserves
    fee: u128, // fee charged in this bin
    out: u128, // output taken from the bin
}

impl Book {
    pub fn swap(&mut self, side: Side, amount_in: u128, min_out: u128) -> Result<SwapResult, Error> {
        if amount_in == 0 {
            return Err(Error::ZeroAmount);
        }
        let (steps, left) = self.plan(side, amount_in);
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

    /// Bins that can fill this side, in walking order. Collected into a Vec
    /// so we stop borrowing `self.bins` before the loop reads other fields.
    fn walk(&self, side: Side) -> Vec<i32> {
        match side {
            Side::Buy => self.bins.range(self.active..).filter(|(_, b)| b.token > 0).map(|(k, _)| *k).collect(),
            Side::Sell => self.bins.range(..=self.active).rev().filter(|(_, b)| b.quote > 0).map(|(k, _)| *k).collect(),
        }
    }

    /// Read-only: how the trade would fill. Returns the steps and unspent input.
    fn plan(&self, side: Side, amount_in: u128) -> (Vec<Step>, u128) {
        let fee_bps = self.cfg.fee_bps;
        let mut left = amount_in;
        let mut steps = Vec::new();
        for bin in self.walk(side) {
            let (b, price) = (&self.bins[&bin], self.price(bin));
            let reserve = if side == Side::Buy { b.token } else { b.quote };
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
        let mut split = split_fee(s.fee, self.cfg.shares);
        let b = self.bins.get_mut(&s.bin).expect("planned bin exists");
        if b.shares == 0 {
            // Only rounding dust is left here with no LPs: their slice goes to the pot.
            split.pot += split.lps;
            split.lps = 0;
        }
        match side {
            Side::Buy => {
                b.token -= s.out;
                b.quote += s.net;
                b.acc_quote = accrue(b.acc_quote, split.lps, b.shares);
                self.pots.pot_quote += split.pot;
                self.pots.creator_quote += split.creator;
            }
            Side::Sell => {
                b.quote -= s.out;
                b.token += s.net;
                b.acc_token = accrue(b.acc_token, split.lps, b.shares);
                self.pots.pot_token += split.pot;
                self.pots.creator_token += split.creator;
            }
        }
    }
}
