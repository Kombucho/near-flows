//! Creator slice: 10% of every fee, shared by LOCKED liquidity pro-rata.
//! Whoever keeps the most liquidity locked earns the most. If a dev walks
//! away, a community that locks more takes over automatically (CTO).
//! When nobody is locked, the slice flows into the community pot.

use math::acc::{accrue, owed};

use crate::Book;

impl Book {
    /// Move a position's earned creator slice into its `owed` fields.
    pub(crate) fn settle_creator(&mut self, id: u64) {
        let (acc_q, acc_t) = (self.creator.acc_q, self.creator.acc_t);
        let p = self.positions.get_mut(&id).expect("position exists");
        p.creator_owed_q += owed(p.creator_weight, acc_q, p.snap_cq);
        p.creator_owed_t += owed(p.creator_weight, acc_t, p.snap_ct);
        p.snap_cq = acc_q;
        p.snap_ct = acc_t;
    }

    /// Stop a position earning the creator slice (lock expired or withdrawn).
    pub(crate) fn drop_creator_weight(&mut self, id: u64) {
        self.settle_creator(id);
        let p = self.positions.get_mut(&id).expect("position exists");
        self.creator.total_weight -= p.creator_weight;
        p.creator_weight = 0;
        self.creator.locked.remove(&id);
    }

    /// Positions whose lock has ended stop earning. Called by the crank.
    pub(crate) fn expire_locks(&mut self, now_ns: u64) {
        let expired: Vec<u64> = self.creator.locked.iter().copied()
            .filter(|id| self.positions[id].locked_until_ns <= now_ns)
            .collect();
        for id in expired {
            self.drop_creator_weight(id);
        }
    }

    /// Hand the collected creator slice to locked liquidity, or to the pot.
    pub(crate) fn distribute_creator(&mut self) {
        let (q, t) = (self.pots.creator_quote, self.pots.creator_token);
        self.pots.creator_quote = 0;
        self.pots.creator_token = 0;
        let c = &mut self.creator;
        if c.total_weight == 0 {
            self.pots.pot_quote += q;
            self.pots.pot_token += t;
            return;
        }
        c.acc_q = accrue(c.acc_q, q, c.total_weight);
        c.acc_t = accrue(c.acc_t, t, c.total_weight);
    }
}
