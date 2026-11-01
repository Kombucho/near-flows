//! Hold-to-earn vault: stake the coin, keep 100% exposure, earn dividends,
//! and steer the community pot with a standing vote.

use std::collections::BTreeMap;

use math::acc::{accrue, owed};
use math::fees::{PotVotes, DEFAULT_MIX};

use crate::{Book, Error};

/// A vote: bps for (dividend, burn, deepen). Must add up to 10_000.
pub type Vote = [u16; 3];
pub const DEFAULT_VOTE: Vote = [DEFAULT_MIX.dividend as u16, DEFAULT_MIX.burn as u16, DEFAULT_MIX.deepen as u16];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct Staker {
    pub stake: u128,
    pub vote: Vote,
    pub snap_q: u128, // dividend counters when last settled
    pub snap_t: u128,
    pub owed_q: u128, // settled dividends not yet claimed
    pub owed_t: u128,
    pub pending: u128, // unstaked, waiting for the delay
    pub release_ns: u64,
}

#[derive(Clone, Debug, Default)]
#[cfg_attr(feature = "borsh", derive(borsh::BorshSerialize, borsh::BorshDeserialize))]
pub struct Vault {
    pub total_stake: u128,
    pub acc_q: u128,
    pub acc_t: u128,
    pub votes: PotVotes, // stake-weighted totals of every staker's vote
    pub stakers: BTreeMap<String, Staker>,
}

impl Vault {
    /// Move earned dividends into `owed` and bring snapshots up to date.
    fn settle(&mut self, who: &str) {
        let (acc_q, acc_t) = (self.acc_q, self.acc_t);
        if let Some(s) = self.stakers.get_mut(who) {
            s.owed_q += owed(s.stake, acc_q, s.snap_q);
            s.owed_t += owed(s.stake, acc_t, s.snap_t);
            s.snap_q = acc_q;
            s.snap_t = acc_t;
        }
    }

    /// Add (+) or remove (-) one staker's weight from the vote totals.
    fn tally(&mut self, stake: u128, vote: Vote, add: bool) {
        let w = |bps: u16| stake * bps as u128;
        let (d, b, x) = (w(vote[0]), w(vote[1]), w(vote[2]));
        let v = &mut self.votes;
        if add {
            v.dividend += d; v.burn += b; v.deepen += x;
        } else {
            v.dividend -= d; v.burn -= b; v.deepen -= x;
        }
    }

    /// Pay dividends to all stakers. Returns false if nobody is staked.
    pub(crate) fn distribute(&mut self, q: u128, t: u128) -> bool {
        if self.total_stake == 0 {
            return false;
        }
        self.acc_q = accrue(self.acc_q, q, self.total_stake);
        self.acc_t = accrue(self.acc_t, t, self.total_stake);
        true
    }

    /// Everything a staker could take out right now or later:
    /// (tokens staked + pending, dividends quote, dividends token).
    pub fn assets(&self, who: &str) -> (u128, u128, u128) {
        let Some(s) = self.stakers.get(who) else { return (0, 0, 0) };
        let dq = s.owed_q + owed(s.stake, self.acc_q, s.snap_q);
        let dt = s.owed_t + owed(s.stake, self.acc_t, s.snap_t);
        (s.stake + s.pending, dq, dt)
    }
}

impl Book {
    /// Stake tokens (already transferred to the contract).
    pub fn stake(&mut self, who: &str, amount: u128) -> Result<(), Error> {
        if !self.cfg.native {
            return Err(Error::NotNative); // staking only means something on native coins
        }
        if amount == 0 {
            return Err(Error::ZeroAmount);
        }
        let v = &mut self.vault;
        let (acc_q, acc_t) = (v.acc_q, v.acc_t);
        let s = v.stakers.entry(who.to_string()).or_insert_with(|| Staker {
            vote: DEFAULT_VOTE, snap_q: acc_q, snap_t: acc_t, ..Default::default()
        });
        let vote = s.vote;
        v.settle(who);
        v.stakers.get_mut(who).expect("just inserted").stake += amount;
        v.total_stake += amount;
        v.tally(amount, vote, true);
        Ok(())
    }

    /// Change your standing vote. Takes effect from the next crank.
    pub fn set_vote(&mut self, who: &str, vote: Vote) -> Result<(), Error> {
        if vote.iter().map(|&x| x as u32).sum::<u32>() != 10_000 {
            return Err(Error::BadVote);
        }
        let v = &mut self.vault;
        let s = v.stakers.get(who).ok_or(Error::NotStaker)?;
        let (stake, old) = (s.stake, s.vote);
        v.tally(stake, old, false);
        v.tally(stake, vote, true);
        v.stakers.get_mut(who).expect("exists").vote = vote;
        Ok(())
    }

    /// Stop staking `amount`. It stops earning and voting now and can be
    /// withdrawn after the unstake delay.
    pub fn request_unstake(&mut self, who: &str, amount: u128, now_ns: u64) -> Result<(), Error> {
        let delay = self.cfg.unstake_delay_ns;
        let v = &mut self.vault;
        let s = v.stakers.get(who).ok_or(Error::NotStaker)?;
        if amount == 0 || amount > s.stake {
            return Err(Error::NotEnoughStake);
        }
        let vote = s.vote;
        v.settle(who);
        v.tally(amount, vote, false);
        v.total_stake -= amount;
        let s = v.stakers.get_mut(who).expect("exists");
        s.stake -= amount;
        s.pending += amount;
        s.release_ns = now_ns + delay; // a new request restarts the wait for all pending
        Ok(())
    }

    /// Take back unstaked tokens once the delay has passed. Returns the amount.
    pub fn withdraw_unstaked(&mut self, who: &str, now_ns: u64) -> Result<u128, Error> {
        let s = self.vault.stakers.get_mut(who).ok_or(Error::NotStaker)?;
        if now_ns < s.release_ns {
            return Err(Error::Locked { until_ns: s.release_ns });
        }
        Ok(std::mem::take(&mut s.pending)) // returns pending and sets it to 0
    }

    /// Claim dividends: (quote, token).
    pub fn claim_dividends(&mut self, who: &str) -> Result<(u128, u128), Error> {
        self.vault.settle(who);
        let s = self.vault.stakers.get_mut(who).ok_or(Error::NotStaker)?;
        Ok((std::mem::take(&mut s.owed_q), std::mem::take(&mut s.owed_t)))
    }
}
