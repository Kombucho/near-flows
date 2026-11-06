//! Market prices, so a swap's minimum follows the market instead of a number
//! the owner typed once (which arbitrage bots would happily fill against).
//!
//! A swap node may carry a `Market` floor: "selling feed A for feed B, accept at
//! most `slippage_bps` below the oracle price". Before the vault signs or runs
//! such a swap it reads the prices itself, and the minimum becomes
//!     max(keeper's quote, owner's hard floor, oracle price x (1 - slippage)).
//! The keeper's quote is never the reference: it can only ask for MORE.
//!
//! Prices come from Pyth (pull oracle: anyone pushes a price signed by Pyth's
//! publishers and checked by Wormhole guardians; the vault only reads). For
//! every feed the vault checks, and refuses the swap otherwise:
//! - fresh: published within `max_age_s` (Pyth itself refuses older),
//! - tight: confidence interval within `max_conf_bps` of the price,
//! - confirmed: a second reading (Pyth's EMA price, or another Pyth-compatible
//!   contract) within `tolerance_bps`: a price that just jumped or got stuck fails.
//! No price, no trade: there is no fallback to anyone's word.

use near_sdk::json_types::{I64, U64};
use near_sdk::{env, near, require, AccountId, Gas, NearToken, Promise};

use math::wide::U256;

use crate::{Vault, VaultExt};

const GAS_READ: Gas = Gas::from_tgas(5);
const BPS: u128 = 10_000;

/// The second reading that must agree with a feed's price.
#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Backup {
    /// The same contract's EMA price for the same feed.
    Ema,
    /// Another Pyth-compatible contract and feed id.
    Other { contract: AccountId, price_id: String },
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    /// What templates and recipes call it, e.g. "eth", "nvda", "usdc".
    pub name: String,
    /// The Pyth contract (pyth-oracle.near / pyth-oracle.testnet).
    pub contract: AccountId,
    /// The Pyth price feed id, hex (e.g. ETH/USD ff61491a...).
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

/// A floor that follows the market: sell `sell`, buy `buy`, token decimals on
/// the chain where the swap happens, and the most the swap may give up.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Market {
    pub sell: String,
    pub buy: String,
    pub dec_in: u8,
    pub dec_out: u8,
    pub slippage_bps: u32,
}

/// What a Pyth contract answers (I64 / U64 are JSON strings).
#[near(serializers = [json])]
pub struct PythPrice {
    pub price: I64,
    pub conf: U64,
    pub expo: i32,
    pub publish_time: i64,
}

#[near]
impl Vault {
    /// Owner: the price feeds swap floors may use.
    pub fn oracle_setup(&mut self, config: OracleConfig) {
        self.assert_owner();
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
        self.oracle = Some(config);
    }

    pub fn get_oracle(&self) -> Option<OracleConfig> {
        self.oracle.clone()
    }
}

fn check_id(id: &str) {
    require!(id.len() == 64 && hex::decode(id).is_ok(), "price_id: 32 bytes, hex");
}

impl Vault {
    fn feed(&self, name: &str) -> Feed {
        let cfg = self.oracle.as_ref().unwrap_or_else(|| env::panic_str("refused: no oracle set up (oracle_setup)"));
        cfg.feeds.iter().find(|f| f.name == name).cloned().unwrap_or_else(|| env::panic_str(&format!("refused: no price feed '{name}'")))
    }

    /// Read every feed in `names` (each: its price, then its backup), as one joint
    /// promise. The results come back in that order; `oracle_prices` reads them.
    pub(crate) fn oracle_fetch(&self, names: &[String]) -> Promise {
        let age = self.oracle.as_ref().map_or(60, |c| c.max_age_s);
        let read = |contract: &AccountId, method: &str, id: &str| {
            let args = format!(r#"{{"price_id":"{id}","age":{age}}}"#);
            Promise::new(contract.clone()).function_call(method.to_string(), args.into_bytes(), NearToken::from_yoctonear(0), GAS_READ)
        };
        let mut all: Option<Promise> = None;
        for name in names {
            let f = self.feed(name);
            let mut p = read(&f.contract, "get_price_no_older_than", &f.price_id);
            match &f.backup {
                Some(Backup::Ema) => p = p.and(read(&f.contract, "get_ema_price_no_older_than", &f.price_id)),
                Some(Backup::Other { contract, price_id }) => p = p.and(read(contract, "get_price_no_older_than", price_id)),
                None => {}
            }
            all = Some(match all {
                Some(a) => a.and(p),
                None => p,
            });
        }
        all.unwrap_or_else(|| env::panic_str("no feeds to read"))
    }

    /// The USD price of one whole token (scaled 1e18) for each feed in `names`,
    /// from the promise results starting at `first`. Refuses (panics) unless every
    /// price is there, fresh, tight and confirmed by its backup.
    pub(crate) fn oracle_prices(&self, names: &[String], first: u64) -> Vec<u128> {
        let cfg = self.oracle.clone().unwrap_or_else(|| env::panic_str("refused: no oracle"));
        let mut at = first;
        let mut next = |what: &str| -> PythPrice {
            let raw = env::promise_result_checked(at, 1024).unwrap_or_else(|_| env::panic_str(&format!("refused: {what}: oracle unreachable")));
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

/// The least output for `amount_in` (input base units) at these prices:
/// amount_in x p_in / p_out x 10^(dec_out - dec_in) x (1 - slippage), in 256 bits
/// with one division at the end (dividing first would drop digits when dec_in < dec_out).
pub fn market_min(m: &Market, amount_in: u128, p_in: u128, p_out: u128) -> u128 {
    let per_unit = |num: U256, den: U256| {
        let r = num / den;
        if r > U256::from(u128::MAX) { u128::MAX } else { r.as_u128() }
    };
    let (num, den) = ratio(m, p_in, p_out);
    per_unit(num * U256::from(amount_in), den)
}

/// (num, den) with num / den = p_in / p_out x 10^(dec_out - dec_in) x (1 - slippage).
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
    const E18: u128 = 1_000_000_000_000_000_000;

    fn p(price: i64, expo: i32) -> PythPrice {
        PythPrice { price: I64(price), conf: U64(0), expo, publish_time: 0 }
    }

    #[test]
    fn floors_from_prices() {
        // ETH $2,224.50 (Pyth: 222450000000 x 1e-8), USDC $1.0001.
        let eth = usd_e18(&p(222_450_000_000, -8)).unwrap();
        let usdc = usd_e18(&p(100_010_000, -8)).unwrap();
        assert_eq!(eth, 2_224_500_000_000_000_000_000);
        let m = Market { sell: "eth".into(), buy: "usdc".into(), dec_in: 18, dec_out: 6, slippage_bps: 50 };
        // 0.1 WETH -> at least 222.45 / 1.0001 x 0.995 = 221.316 USDC.
        let min = market_min(&m, 100_000_000_000_000_000, eth, usdc);
        assert_eq!(min / 10_000, 22_131); // 221.31 USDC (6 decimals)
        // The same floor per unit of input, scaled 1e24, gives the same minimum.
        let per_in = market_per_in(&m, eth, usdc, 1_000_000_000_000_000_000_000_000);
        assert_eq!(crate::mul_div(100_000_000_000_000_000, per_in, 1_000_000_000_000_000_000_000_000) / 10_000, 22_131);
        // The other way: 1,000 USDC -> at least 1000 / 2224.5 x 0.995 = 0.4473 WETH.
        let back = Market { sell: "usdc".into(), buy: "eth".into(), dec_in: 6, dec_out: 18, slippage_bps: 50 };
        assert_eq!(market_min(&back, 1_000_000_000, usdc, eth) / 100_000_000_000_000, 4_473);
        // USDC (6 decimals) -> ETH (18): 40 USDC ($1.0001) at $2,224.50, 0.5% slippage = 0.01789345... ETH, every digit kept.
        assert_eq!(market_min(&back, 40_000_000, usdc, eth), 17_893_450_213_531_130); // exact, from Python fractions
        // Limits: zero amount -> zero; full slippage -> zero; a negative price is no price.
        assert_eq!(market_min(&m, 0, eth, usdc), 0);
        assert_eq!(market_min(&Market { slippage_bps: 10_000, ..m.clone() }, 1, eth, usdc), 0);
        assert!(usd_e18(&p(-1, -8)).is_none() && usd_e18(&p(0, -8)).is_none());
        // Positive exponents and huge ones don't overflow silently.
        assert_eq!(usd_e18(&p(5, 2)).unwrap(), 500 * E18);
        assert!(usd_e18(&p(i64::MAX, 30)).is_none());
    }
}
