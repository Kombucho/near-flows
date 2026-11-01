//! The simulated world around one market: wallets, the contract's real
//! balances, and helpers that move tokens exactly as the contract would.
//!
//! Units: 1 NEAR = 1 token-unit = 10^18 (both sides use 18 decimals here).

use engine::{Book, Error, Rung, Side, SwapResult};

pub const ONE: u128 = 1_000_000_000_000_000_000;
pub const HOUR: u64 = 3_600 * 1_000_000_000;

pub fn near(x: f64) -> u128 {
    (x.max(0.0) * ONE as f64) as u128
}
pub fn to_f(x: u128) -> f64 {
    x as f64 / ONE as f64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Spot,        // hold everything in the wallet
    Ladder,      // 50% wallet, 50% take-profit ladder
    Stake,       // stake everything
    LadderStake, // 50% ladder, 50% staked
}
pub const STRATEGIES: [Strategy; 4] = [Strategy::Spot, Strategy::Ladder, Strategy::Stake, Strategy::LadderStake];

pub struct Holder {
    pub name: String,
    pub strategy: Strategy,
    pub deposit: f64, // NEAR put into the auction
    pub wallet_t: u128,
    pub wallet_q: u128,
    pub ladder: Vec<u64>,
}

/// Counters for things that should never happen.
#[derive(Default, Clone, Debug)]
pub struct Faults {
    pub insolvent: u32,   // obligations exceeded real balances
    pub unexpected: u32,  // an engine error nobody should hit
    pub first: Option<String>,
}

pub struct World {
    pub book: Book,
    pub now: u64,
    pub contract_q: u128, // what the contract really holds
    pub contract_t: u128,
    pub faults: Faults,
    pub volume_q: u128,
}

impl World {
    pub fn fault(&mut self, what: String) {
        self.faults.unexpected += 1;
        self.faults.first.get_or_insert(what);
    }

    /// Errors that are normal market life (no liquidity on one side, etc.).
    fn expected(e: &Error) -> bool {
        matches!(e, Error::NoLiquidity | Error::Slippage { .. } | Error::ZeroAmount | Error::TooSoon { .. })
    }

    fn check(&mut self, r: Result<(), Error>, ctx: &str) {
        if let Err(e) = r {
            if !Self::expected(&e) {
                self.fault(format!("{ctx}: {e:?}"));
            }
        }
    }

    pub fn price(&self) -> f64 {
        to_f(self.book.price(self.book.active))
    }

    /// Buy through the market. Returns (tokens to wallet, quote spent, ladder ids).
    /// Buy with a slippage limit, like a real wallet: refuse fills worse than
    /// `slip` (e.g. 0.05 = 5%) beyond the fee at the current price.
    pub fn buy(&mut self, who: &str, quote: u128, ladder: &[Rung], slip: f64) -> Option<(u128, u128, Vec<u64>)> {
        let fee = self.book.cfg.fee_bps as f64 / 10_000.0;
        let min_out = near(to_f(quote) * (1.0 - fee) * (1.0 - slip) / self.price());
        match self.book.buy(who, quote, min_out, ladder, self.now) {
            Ok(b) => {
                self.contract_q += b.swap.used;
                self.contract_t -= b.kept; // the laddered part stays in the contract
                self.volume_q += b.swap.used;
                Some((b.kept, b.swap.used, b.ladder))
            }
            Err(e) => { self.check(Err(e), "buy"); None }
        }
    }

    /// Sell tokens. Returns the swap result.
    /// Sell with a slippage limit (1.0 = accept anything, used for panic exits).
    pub fn sell(&mut self, tokens: u128, slip: f64) -> Option<SwapResult> {
        if tokens == 0 {
            return None;
        }
        let fee = self.book.cfg.fee_bps as f64 / 10_000.0;
        let min_out = near(to_f(tokens) * self.price() * (1.0 - fee) * (1.0 - slip));
        match self.book.swap(Side::Sell, tokens, min_out) {
            Ok(r) => {
                self.contract_t += r.used;
                self.contract_q -= r.out;
                self.volume_q += r.out;
                Some(r)
            }
            Err(e) => { self.check(Err(e), "sell"); None }
        }
    }

    pub fn crank(&mut self) {
        let r = self.book.crank(self.now).map(|_| ());
        self.check(r, "crank");
    }

    pub fn stake(&mut self, h: &mut Holder, amount: u128) {
        if amount > 0 && self.book.stake(&h.name, amount).is_ok() {
            h.wallet_t -= amount;
            self.contract_t += amount;
        }
    }

    pub fn ladder(&mut self, h: &mut Holder, tokens: u128, rungs: &[Rung]) {
        match self.book.place_ladder(&h.name, tokens, rungs, 0, self.now) {
            Ok((ids, placed)) => {
                h.wallet_t -= placed;
                self.contract_t += placed;
                h.ladder.extend(ids);
            }
            Err(e) => self.check(Err(e), "ladder"),
        }
    }

    /// Close every ladder position and collect dividends into the wallet.
    pub fn close_all(&mut self, h: &mut Holder) {
        for id in std::mem::take(&mut h.ladder) {
            match self.book.withdraw(id, &h.name, self.now) {
                Ok(w) => {
                    let (t, q) = (w.token + w.fee_token, w.quote + w.fee_quote);
                    h.wallet_t += t; h.wallet_q += q;
                    self.contract_t -= t; self.contract_q -= q;
                }
                Err(e) => { self.check(Err(e), "withdraw"); h.ladder.push(id); }
            }
        }
        if let Ok((q, t)) = self.book.claim_dividends(&h.name) {
            h.wallet_q += q; h.wallet_t += t;
            self.contract_q -= q; self.contract_t -= t;
        }
    }

    /// Solvency: what the market owes must never exceed what it holds.
    pub fn audit(&mut self) {
        let (oq, ot) = self.book.obligations();
        let held_t = self.contract_t.saturating_sub(self.book.burned);
        if oq > self.contract_q || ot > held_t {
            self.faults.insolvent += 1;
            self.faults.first.get_or_insert(format!("insolvent at {}h: owes ({oq},{ot}) holds ({},{held_t})", self.now / HOUR, self.contract_q));
        }
    }

    /// Everything a holder could walk away with, in NEAR, valued at `p`.
    pub fn holder_value(&self, h: &Holder, p: f64) -> f64 {
        let mut v = to_f(h.wallet_q) + to_f(h.wallet_t) * p;
        for id in &h.ladder {
            if let Ok((t, q)) = self.book.position_assets(*id) {
                v += to_f(q) + to_f(t) * p;
            }
        }
        let (staked, dq, dt) = self.book.vault.assets(&h.name);
        v + to_f(dq) + to_f(staked + dt) * p
    }
}
