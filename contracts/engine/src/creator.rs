//! Creator slice: 10% of every fee, shared by LOCKED liquidity pro-rata.
//! Whoever keeps the most liquidity locked earns the most. If a dev walks
//! away, a community that locks more takes over automatically (CTO).
//! When nobody is locked, the slice flows into the community pot.

use math::acc::{accrue, owed};

use crate::Book;

const DAY_NS: u64 = 86_400 * 1_000_000_000;

/// Day files and entries the crank may walk per call.
const MAX_EXPIRE_STEPS: u32 = 200;

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
        // Its entry in the day file stays; expiry skips positions with no weight.
    }

    /// Positions whose lock has ended stop earning. Called by the crank.
    /// Locks are filed under the day they END rounded UP, so everything in
    /// the file of a day that has started has expired (a lock ending mid-day
    /// keeps earning until that day is over, at most a day late; withdrawing
    /// stops it at once). Walks the day files in order from where it stopped last time, at most
    /// MAX_EXPIRE_STEPS entries per crank so gas stays bounded; a backlog
    /// simply finishes over the next cranks.
    pub(crate) fn expire_locks(&mut self, now_ns: u64) {
        let today = now_ns / DAY_NS;
        let (mut day, mut i) = self.creator.expiry_cursor;
        if day == 0 {
            return; // nothing was ever filed
        }
        let mut steps = 0;
        while day <= today && steps < MAX_EXPIRE_STEPS {
            let n = self.creator.lock_count.get(&day).copied().unwrap_or(0);
            if i >= n {
                if day == today {
                    break; // done for now; resume here
                }
                (day, i) = (day + 1, 0);
                steps += 1;
                continue;
            }
            let id = self.creator.lock_items[&(day, i)];
            let still = self.positions.get(&id).map_or(false, |p| p.creator_weight > 0 && p.locked_until_ns <= now_ns);
            if still {
                self.drop_creator_weight(id);
            }
            i += 1;
            steps += 1;
        }
        self.creator.expiry_cursor = (day, i);
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
