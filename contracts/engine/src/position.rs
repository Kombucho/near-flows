//! Claiming LP fees and withdrawing liquidity.

use math::acc::owed;
use math::wide::mul_div_down;

use crate::{Book, Error};

#[derive(Debug, PartialEq, Eq)]
pub struct Withdrawal {
    pub token: u128,
    pub quote: u128,
    pub fee_quote: u128,
    pub fee_token: u128,
}

impl Book {
    /// Fees a position can claim right now: (quote, token).
    pub fn claimable(&self, id: u64) -> Result<(u128, u128), Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        let b = &self.bins[&p.bin];
        Ok((owed(p.shares, b.acc_quote, p.snap_quote), owed(p.shares, b.acc_token, p.snap_token)))
    }

    /// Settle fees: returns (quote, token) to pay out and moves the snapshots up,
    /// so the same fees can never be claimed twice.
    pub fn claim(&mut self, id: u64, caller: &str) -> Result<(u128, u128), Error> {
        let fees = self.claimable(id)?;
        let p = self.positions.get_mut(&id).expect("checked by claimable");
        if p.owner != caller {
            return Err(Error::NotOwner);
        }
        let b = &self.bins[&p.bin];
        p.snap_quote = b.acc_quote;
        p.snap_token = b.acc_token;
        Ok(fees)
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
