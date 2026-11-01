//! Claiming fees and withdrawing liquidity.

use math::acc::owed;
use math::wide::mul_div_down;

use crate::{Book, Error};

#[derive(Debug, PartialEq, Eq)]
pub struct Withdrawal {
    pub token: u128,
    pub quote: u128,
    pub fee_quote: u128, // LP fees + creator slice
    pub fee_token: u128,
}

impl Book {
    /// Fees a position can claim right now (LP fees + creator slice): (quote, token).
    pub fn claimable(&self, id: u64) -> Result<(u128, u128), Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        let b = &self.bins[&p.bin];
        let c = &self.creator;
        let q = owed(p.shares, b.acc_quote, p.snap_quote)
            + p.creator_owed_q + owed(p.creator_weight, c.acc_q, p.snap_cq);
        let t = owed(p.shares, b.acc_token, p.snap_token)
            + p.creator_owed_t + owed(p.creator_weight, c.acc_t, p.snap_ct);
        Ok((q, t))
    }

    /// Settle and pay out fees: (quote, token). Snapshots move up, so the
    /// same fees can never be claimed twice.
    pub fn claim(&mut self, id: u64, caller: &str) -> Result<(u128, u128), Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        if p.owner != caller {
            return Err(Error::NotOwner);
        }
        let fees = self.claimable(id)?;
        self.settle_creator(id);
        let b = &self.bins[&self.positions[&id].bin];
        let (aq, at) = (b.acc_quote, b.acc_token);
        let p = self.positions.get_mut(&id).expect("exists");
        p.snap_quote = aq;
        p.snap_token = at;
        p.creator_owed_q = 0;
        p.creator_owed_t = 0;
        Ok(fees)
    }

    /// What a position is worth if closed now: (token, quote), fees included.
    pub fn position_assets(&self, id: u64) -> Result<(u128, u128), Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        let b = &self.bins[&p.bin];
        let (fq, ft) = self.claimable(id)?;
        let token = mul_div_down(b.token, p.shares, b.shares);
        let quote = mul_div_down(b.quote, p.shares, b.shares);
        Ok((token + ft, quote + fq))
    }

    /// Close a position: its fees plus its pro-rata slice of the bin's reserves.
    pub fn withdraw(&mut self, id: u64, caller: &str, now_ns: u64) -> Result<Withdrawal, Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        if p.owner != caller {
            return Err(Error::NotOwner);
        }
        if now_ns < p.locked_until_ns {
            return Err(Error::Locked { until_ns: p.locked_until_ns });
        }
        if p.creator_weight > 0 {
            self.drop_creator_weight(id);
        }
        let (fee_quote, fee_token) = self.claim(id, caller)?;
        let p = self.positions.remove(&id).expect("exists");
        let b = self.bins.get_mut(&p.bin).expect("position's bin exists");
        // Round DOWN: any dust stays in the bin for the remaining LPs.
        let token = mul_div_down(b.token, p.shares, b.shares);
        let quote = mul_div_down(b.quote, p.shares, b.shares);
        b.token -= token;
        b.quote -= quote;
        b.shares -= p.shares;
        Ok(Withdrawal { token, quote, fee_quote, fee_token })
    }
}
