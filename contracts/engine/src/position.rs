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
        let creator_q = p.creator_owed_q + owed(p.creator_weight, c.acc_q, p.snap_cq);
        let creator_t = p.creator_owed_t + owed(p.creator_weight, c.acc_t, p.snap_ct);
        if p.take_profit {
            // Fill proceeds (in the drawer) + maker fees, plus any creator slice.
            return Ok((self.tp_earned(p) + creator_q, creator_t));
        }
        if p.bid {
            // Coins bought (in the drawer) + maker fees.
            return Ok((creator_q, self.bid_earned(p) + creator_t));
        }
        let q = owed(p.shares, b.acc_quote, p.snap_quote) + creator_q;
        let t = owed(p.shares, b.acc_token, p.snap_token) + creator_t;
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
        let p = &self.positions[&id];
        let b = &self.bins[&p.bin];
        let (aq, at) = if p.take_profit {
            (self.tp_acc_for(p), 0)
        } else if p.bid {
            (self.bid_acc_for(p), 0)
        } else {
            (b.acc_quote, b.acc_token)
        };
        let p = self.positions.get_mut(&id).expect("exists");
        p.snap_quote = aq;
        p.snap_token = at;
        p.creator_owed_q = 0;
        p.creator_owed_t = 0;
        p.tp_owed = 0;
        Ok(fees)
    }

    /// What a position is worth if closed now: (token, quote), fees included.
    pub fn position_assets(&self, id: u64) -> Result<(u128, u128), Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        let b = &self.bins[&p.bin];
        let (fq, ft) = self.claimable(id)?;
        if p.take_profit {
            return Ok((self.tp_remaining(p) + ft, fq));
        }
        if p.bid {
            return Ok((ft, self.bid_remaining(p) + fq));
        }
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
        if self.positions[&id].bid {
            let (fee_quote, fee_token) = self.claim(id, caller)?;
            let (quote, _) = self.bid_close(id);
            return Ok(Withdrawal { token: 0, quote, fee_quote, fee_token });
        }
        if self.positions[&id].take_profit {
            // Creator slice first (claim), then the order itself: unsold tokens back,
            // proceeds already counted in the claim.
            let (fee_quote, fee_token) = self.claim(id, caller)?;
            let (token, _) = self.tp_close(id);
            return Ok(Withdrawal { token, quote: 0, fee_quote, fee_token });
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

/// Result of `settle`: who gets paid, how much, and whether the position closed.
#[derive(Debug, PartialEq, Eq)]
pub struct Settlement {
    pub owner: String,
    pub quote: u128,
    pub token: u128,
    pub closed: bool,
}

impl Book {
    /// Pay a position's earnings to its OWNER. Anyone may trigger it (e.g. a
    /// keeper), because the money can only go to the owner. A take-profit
    /// that has fully sold and is not locked is also closed, freeing storage.
    /// The protocol's own positions cannot be settled.
    pub fn settle(&mut self, id: u64, now_ns: u64) -> Result<Settlement, Error> {
        let p = self.positions.get(&id).ok_or(Error::NoPosition)?;
        let (owner, wheel) = (p.owner.clone(), p.bid && p.rearm > 0);
        if owner == crate::book::PROTOCOL {
            return Err(Error::NotOwner);
        }
        if wheel {
            // A wheel bid's coins are not paid out: they become sell liquidity above.
            self.rearm(id, now_ns)?;
            let p = &self.positions[&id];
            if self.bid_remaining(p) == 0 && now_ns >= p.locked_until_ns {
                let w = self.withdraw(id, &owner, now_ns)?; // fully used: close it
                return Ok(Settlement { owner, quote: w.quote + w.fee_quote, token: w.token + w.fee_token, closed: true });
            }
            return Ok(Settlement { owner, quote: 0, token: 0, closed: false });
        }
        let (mut quote, mut token) = self.claim(id, &owner)?;
        let p = &self.positions[&id];
        let sold_out = (p.take_profit && self.tp_remaining(p) == 0) || (p.bid && p.rearm == 0 && self.bid_remaining(p) == 0);
        let done = sold_out && now_ns >= p.locked_until_ns;
        if done {
            let w = self.withdraw(id, &owner, now_ns)?;
            quote += w.quote + w.fee_quote;
            token += w.token + w.fee_token;
        }
        Ok(Settlement { owner, quote, token, closed: done })
    }

    /// Positions with something to pay out (for keepers), skipping the protocol's.
    pub fn settleable(&self, limit: usize) -> Vec<u64> {
        self.positions.iter()
            .filter(|(_, p)| p.owner != crate::book::PROTOCOL)
            .filter(|(id, _)| self.claimable(**id).map(|(q, t)| q + t > 0).unwrap_or(false))
            .map(|(id, _)| *id)
            .take(limit)
            .collect()
    }
}
