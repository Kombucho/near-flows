//! Read-only views. Free to call (no transaction), used by the front end.
//! Amounts are `U128` strings so JavaScript can read them without losing digits.

use near_sdk::json_types::U128;
use near_sdk::{near, AccountId};

use crate::{Market, MarketExt};

#[near(serializers = [json])]
pub struct ConfigView {
    pub token: AccountId,
    pub quote: AccountId,
    pub native: bool,
    pub fee_bps: u16,
    pub launched: bool,
    pub paused: bool,
    pub step_bps: u16,
    pub base_price: U128, // quote per token, 1e18 fixed-point
    pub active_bin: i32,
    pub price: U128, // current price, same format
    pub current_fee_bps: u16, // what a small trade pays right now (dynamic)
}

#[near(serializers = [json])]
pub struct BinView {
    pub id: i32,
    pub price: U128,
    pub token: U128,    // two-way LP tokens (asks)
    pub quote: U128,    // two-way LP quote (bids)
    pub tp_token: U128, // one-way take-profit tokens waiting
    pub bid_quote: U128, // one-way bids (wheel buys) waiting
}

#[near(serializers = [json])]
pub struct PositionView {
    pub id: u64,
    pub owner: String,
    pub bin: i32,
    pub take_profit: bool,
    pub bid: bool,  // one-way bid (wheel buy)
    pub rearm: u8,  // wheel exit preset: bought coins become asks (0 = paid out instead)
    pub locked_until_ns: u64,
    pub token: U128, // what it would return if closed now (incl. earnings)
    pub quote: U128,
}

#[near(serializers = [json])]
pub struct MarketStats {
    pub burned: U128,
    pub total_staked: U128,
    pub votes_dividend: U128,
    pub votes_burn: U128,
    pub votes_deepen: U128,
    pub positions: u64,
    /// Position ids run 0..next_id: keepers and auditors page through them.
    pub next_id: u64,
    pub creator_weight: U128,
    pub daily_volume: U128,       // smoothed, in quote units
    pub protocol_fee_bps_now: u16, // share of each fee going to the protocol right now
    pub protocol_pending_quote: U128,
    pub protocol_pending_token: U128,
}

#[near(serializers = [json])]
pub struct StakerView {
    pub staked: U128, // staked + pending unstake
    pub dividends_quote: U128,
    pub dividends_token: U128,
    pub vote: [u16; 3],
    pub release_ns: u64,
}

#[near]
impl Market {
    pub fn get_config(&self) -> ConfigView {
        let (step, base, active, price, fee) = match &self.book {
            Some(b) => (b.cfg.step_bps, b.cfg.base_price, b.active, b.price(b.active), b.current_fee_bps(near_sdk::env::block_timestamp())),
            None => (0, 0, 0, 0, self.fee_bps),
        };
        ConfigView {
            token: self.token.clone(), quote: self.quote.clone(), native: self.native, fee_bps: self.fee_bps,
            launched: self.book.is_some(), paused: self.paused, step_bps: step,
            base_price: U128(base), active_bin: active, price: U128(price), current_fee_bps: fee,
        }
    }

    /// Bins with anything in them between `from` and `to` (at most 200).
    pub fn get_bins(&self, from: i32, to: i32) -> Vec<BinView> {
        let b = self.book_ref();
        b.bin_ids.range(from..=to).take(200)
            .map(|id| (id, &b.bins[id]))
            .filter(|(_, x)| x.token + x.quote + x.tp_token + x.bid_quote > 0)
            .map(|(id, x)| BinView {
                id: *id, price: U128(b.price(*id)), token: U128(x.token), quote: U128(x.quote), tp_token: U128(x.tp_token), bid_quote: U128(x.bid_quote),
            })
            .collect()
    }

    pub fn get_positions(&self, account_id: AccountId) -> Vec<PositionView> {
        let b = self.book_ref();
        b.positions_of(account_id.as_str(), 100).into_iter()
            .map(|id| {
                let p = &b.positions[&id];
                let (t, q) = b.position_assets(id).unwrap_or((0, 0));
                PositionView {
                    id, owner: p.owner.clone(), bin: p.bin, take_profit: p.take_profit, bid: p.bid, rearm: p.rearm,
                    locked_until_ns: p.locked_until_ns, token: U128(t), quote: U128(q),
                }
            })
            .collect()
    }

    pub fn get_staker(&self, account_id: AccountId) -> Option<StakerView> {
        let b = self.book_ref();
        let s = b.vault.stakers.get(&account_id.to_string())?;
        let (staked, dq, dt) = b.vault.assets(account_id.as_str());
        Some(StakerView { staked: U128(staked), dividends_quote: U128(dq), dividends_token: U128(dt), vote: s.vote, release_ns: s.release_ns })
    }

    pub fn get_stats(&self) -> MarketStats {
        let b = self.book_ref();
        let v = &b.vault.votes;
        MarketStats {
            burned: U128(b.burned), total_staked: U128(b.vault.total_stake),
            votes_dividend: U128(v.dividend), votes_burn: U128(v.burn), votes_deepen: U128(v.deepen),
            positions: b.positions.len(), next_id: b.next_id, creator_weight: U128(b.creator.total_weight),
            daily_volume: U128(b.daily_volume), protocol_fee_bps_now: b.protocol_bps_now(),
            protocol_pending_quote: U128(b.pots.protocol_quote), protocol_pending_token: U128(b.pots.protocol_token),
        }
    }

    /// Positions with earnings waiting among ids from_id..from_id+scan (scan
    /// at most 300), for keepers to call `settle`. Page up to `next_id`.
    pub fn get_settleable(&self, from_id: u64, scan: u64) -> Vec<u64> {
        self.book_ref().settleable(from_id, scan.min(300))
    }

    /// Solvency check anyone can run: what the market owes, (quote, token).
    /// Paged: sum the pages from_id = 0, count, 2*count, ... up to `next_id`
    /// (count at most 300). The first page (from_id 0) also includes
    /// everything that is not a position: pots, bins, stakes, dividends.
    /// Compare the sum with the contract's balances on the two token contracts.
    pub fn get_obligations(&self, from_id: u64, count: u64) -> (U128, U128) {
        let b = self.book_ref();
        let (mut q, mut t) = b.obligations_of(from_id, count.min(300));
        if from_id == 0 {
            let (bq, bt) = b.obligations_base();
            q += bq;
            t += bt;
        }
        (U128(q), U128(t))
    }
}
