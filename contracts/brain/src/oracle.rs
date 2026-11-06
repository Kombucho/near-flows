//! Market prices, as vault/src/oracle.rs: a swap's minimum follows the market,
//!     max(keeper's quote, owner's hard floor, oracle price x (1 - slippage)).
//! Every feed must be fresh, tight (confidence) and confirmed by its backup
//! (Pyth's EMA, or another Pyth-compatible contract such as a priceboard).
//! No price, no trade.
//!
//! What changed in the split: the reads are views, so the BRAIN issues them
//! (`feed_views`, then `on_read`), never the vault. Same contracts, same
//! arguments, same checks (`prices`), same math (copied verbatim, same tests).

use near_sdk::json_types::{I64, U64};
use near_sdk::{env, near, require, AccountId};

use math::wide::U256;

use crate::Talk;

const BPS: u128 = 10_000;

#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Backup {
    Ema,
    Other { contract: AccountId, price_id: String },
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub name: String,
    pub contract: AccountId,
    pub price_id: String,
    pub backup: Option<Backup>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleConfig {
    pub feeds: Vec<Feed>,
    pub max_age_s: u64,
    pub max_conf_bps: u32,
    pub tolerance_bps: u32,
}

/// A floor that follows the market (same fields and order as the vault's: borsh-identical).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Market {
    pub sell: String,
    pub buy: String,
    pub dec_in: u8,
    pub dec_out: u8,
    pub slippage_bps: u32,
}

#[near(serializers = [json])]
pub struct PythPrice {
    pub price: I64,
    pub conf: U64,
    pub expo: i32,
    pub publish_time: i64,
}

fn check_id(id: &str) {
    require!(id.len() == 64 && hex::decode(id).is_ok(), "price_id: 32 bytes, hex");
}

impl Talk {
    /// Owner: the price feeds floors and triggers may use.
    pub(crate) fn oracle_setup(&mut self, config: OracleConfig) {
        require!(config.feeds.len() <= 32, "at most 32 feeds");
        require!(config.max_age_s >= 1 && config.max_age_s <= 3_600, "max_age_s: 1 s to 1 h");
        require!(config.max_conf_bps <= 1_000 && config.tolerance_bps <= 2_000, "max_conf_bps <= 10%, tolerance_bps <= 20%");
        for f in &config.feeds {
            require!(!f.name.is_empty() && f.name.len() <= 16, "feed name: 1 to 16 characters");
            check_id(&f.price_id);
            if let Some(Backup::Other { price_id, .. }) = &f.backup {
                check_id(price_id);
            }
        }
        self.vs.oracle = Some(config);
    }

    fn feed(&self, name: &str) -> Feed {
        let cfg = self.vs.oracle.as_ref().unwrap_or_else(|| env::panic_str("refused: no oracle set up (oracle_setup)"));
        cfg.feeds.iter().find(|f| f.name == name).cloned().unwrap_or_else(|| env::panic_str(&format!("refused: no price feed '{name}'")))
    }

    /// The views that read every feed in `names` (each: its price, then its
    /// backup), in the order `prices` reads their results.
    pub(crate) fn feed_views(&self, names: &[String]) -> Vec<(AccountId, String, String)> {
        let age = self.vs.oracle.as_ref().map_or(60, |c| c.max_age_s);
        let read = |c: &AccountId, m: &str, id: &str| (c.clone(), m.to_string(), format!(r#"{{"price_id":"{id}","age":{age}}}"#));
        let mut out = Vec::new();
        for name in names {
            let f = self.feed(name);
            out.push(read(&f.contract, "get_price_no_older_than", &f.price_id));
            match &f.backup {
                Some(Backup::Ema) => out.push(read(&f.contract, "get_ema_price_no_older_than", &f.price_id)),
                Some(Backup::Other { contract, price_id }) => out.push(read(contract, "get_price_no_older_than", price_id)),
                None => {}
            }
        }
        out
    }

    /// USD per whole token (1e18) for each feed in `names`, from the promise
    /// results starting at `first`. Refuses unless every price is fresh, tight
    /// and confirmed by its backup.
    pub(crate) fn prices(&self, names: &[String], first: u64) -> Vec<u128> {
        let cfg = self.vs.oracle.clone().unwrap_or_else(|| env::panic_str("refused: no oracle"));
        let mut at = first;
        let mut next = |what: &str| -> PythPrice {
            let raw = crate::result(at, 1024).unwrap_or_else(|| env::panic_str(&format!("refused: {what}: oracle unreachable")));
            at += 1;
            near_sdk::serde_json::from_slice::<Option<PythPrice>>(&raw).ok().flatten()
                .unwrap_or_else(|| env::panic_str(&format!("refused: {what}: no price fresher than {} s", cfg.max_age_s)))
        };
        names
            .iter()
            .map(|name| {
                let f = self.feed(name);
                let p = next(name);
                let usd = usd_e18(&p).unwrap_or_else(|| env::panic_str(&format!("refused: {name}: bad price")));
                require!((p.conf.0 as u128) * BPS <= (p.price.0 as u128) * cfg.max_conf_bps as u128, format!("refused: {name}: price too uncertain"));
                if f.backup.is_some() {
                    let b = usd_e18(&next(name)).unwrap_or_else(|| env::panic_str(&format!("refused: {name}: bad backup price")));
                    let diff = usd.abs_diff(b);
                    require!(diff * BPS <= usd.min(b) * cfg.tolerance_bps as u128, format!("refused: {name}: price and backup disagree"));
                }
                usd
            })
            .collect()
    }

    /// `names` paired with their prices (results from `first`).
    pub(crate) fn named_prices(&self, names: &[String], first: u64) -> Vec<(String, u128)> {
        names.iter().cloned().zip(self.prices(names, first)).collect()
    }
}

/// USD per whole token, scaled 1e18: price x 10^(18 + expo). None if not positive.
pub fn usd_e18(p: &PythPrice) -> Option<u128> {
    if p.price.0 <= 0 {
        return None;
    }
    let v = p.price.0 as u128;
    let e = 18 + p.expo;
    if e >= 0 {
        v.checked_mul(10u128.checked_pow(e as u32)?)
    } else {
        Some(v / 10u128.checked_pow((-e) as u32)?)
    }
}

/// The least output for `amount_in` at these prices, in 256 bits, one division at the end.
pub fn market_min(m: &Market, amount_in: u128, p_in: u128, p_out: u128) -> u128 {
    let (num, den) = ratio(m, p_in, p_out);
    let r = num * U256::from(amount_in) / den;
    if r > U256::from(u128::MAX) { u128::MAX } else { r.as_u128() }
}

fn ratio(m: &Market, p_in: u128, p_out: u128) -> (U256, U256) {
    let mut num = U256::from(p_in) * U256::from(BPS - (m.slippage_bps as u128).min(BPS));
    let mut den = U256::from(p_out.max(1)) * U256::from(BPS);
    if m.dec_out >= m.dec_in {
        num *= U256::from(10u128.pow((m.dec_out - m.dec_in).min(36) as u32));
    } else {
        den *= U256::from(10u128.pow((m.dec_in - m.dec_out).min(36) as u32));
    }
    (num, den)
}

/// The same floor per unit of input, scaled `scale` (1e24 for recipes, 1e12 for Solana).
pub fn market_per_in(m: &Market, p_in: u128, p_out: u128, scale: u128) -> u128 {
    let (num, den) = ratio(m, p_in, p_out);
    let r = num * U256::from(scale) / den;
    if r > U256::from(u128::MAX) { u128::MAX } else { r.as_u128() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(price: i64, expo: i32) -> PythPrice {
        PythPrice { price: I64(price), conf: U64(0), expo, publish_time: 0 }
    }

    /// The vault's own vectors (vault/src/oracle.rs), exact digits from Python fractions.
    #[test]
    fn floors_match_the_vaults_to_the_last_digit() {
        let eth = usd_e18(&p(222_450_000_000, -8)).unwrap();
        let usdc = usd_e18(&p(100_010_000, -8)).unwrap();
        let m = Market { sell: "eth".into(), buy: "usdc".into(), dec_in: 18, dec_out: 6, slippage_bps: 50 };
        assert_eq!(market_min(&m, 100_000_000_000_000_000, eth, usdc) / 10_000, 22_131);
        let back = Market { sell: "usdc".into(), buy: "eth".into(), dec_in: 6, dec_out: 18, slippage_bps: 50 };
        assert_eq!(market_min(&back, 40_000_000, usdc, eth), 17_893_450_213_531_130);
        let meme = usd_e18(&p(51_204, -12)).unwrap();
        let near = usd_e18(&p(200_000_000, -8)).unwrap();
        let buy = Market { sell: "near".into(), buy: "meme".into(), dec_in: 24, dec_out: 6, slippage_bps: 100 };
        assert_eq!(market_min(&buy, 10u128.pow(24), near, meme), 38_668_853_995_781);
        assert!(usd_e18(&p(-1, -8)).is_none() && usd_e18(&p(i64::MAX, 30)).is_none());
    }
}
