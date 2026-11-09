//! Market prices: a swap's minimum follows the market,
//!     max(keeper's quote, owner's hard floor, oracle price x (1 - slippage)).
//! Every feed must be fresh and tight (confidence), and a market floor's feeds
//! must each be confirmed by a backup from a DIFFERENT publisher (Chainlink for a
//! Pyth price, Chainlink on another chain for a Chainlink price), read in the
//! same round and within `tolerance_bps`. No price, no trade.
//!
//! A feed with no independent second source (a pump.fun curve coin has no
//! trusted price at all) is named `single_source`, plainly, never given a
//! made-up backup. It may fire price triggers, but no market floor reads it: a
//! trade in it is bounded by the owner's fixed floor, slippage and size caps,
//! and the vault refuses a market floor on it saying so. (NVDA was one until
//! its backup became the 30-min TWAP of the NVDA/USDG Uniswap v3 pool on
//! Robinhood Chain: a backup only confirms, the floor is the primary's, so a
//! bent TWAP can stop a trade, never cheapen one.)
//!
//! LP shares have no feed of their own: a `share` leaf (evm.rs) prices one
//! from its two tokens' feeds and a per-share constant the owner pins
//! (`ShareValue`). `usd` is a feed name that is exactly one dollar, never read.
//!
//! What changed in the split: the reads are views, so the BRAIN issues them
//! (`feed_views`, then `on_read`), never the vault. Same contracts, same
//! arguments, same checks (`prices`), same math (copied verbatim, same tests).

use near_sdk::json_types::{I64, U128, U64};
use near_sdk::{env, near, require, AccountId};

use math::wide::U256;

use crate::Talk;

const BPS: u128 = 10_000;

#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Backup {
    /// The same asset from a different publisher: another contract, or another
    /// price id on the same board (its Chainlink id behind a relayed Pyth price).
    Other { contract: AccountId, price_id: String },
    /// No independent second source exists: no market floor reads this feed.
    SingleSource,
}

/// What a market floor on a single-source feed is refused with.
pub const SINGLE_SOURCE: &str = "single-source feed: fixed floor + size caps apply (no market floor)";

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Feed {
    pub name: String,
    pub contract: AccountId,
    pub price_id: String,
    /// Required: a second publisher, or `single_source` said out loud.
    pub backup: Backup,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct OracleConfig {
    pub feeds: Vec<Feed>,
    pub max_age_s: u64,
    pub max_conf_bps: u32,
    pub tolerance_bps: u32,
}

/// The feed name `usd`: exactly one dollar, never read. The owner's word that a
/// token is a dollar (USDG, which no oracle prices), written into the template.
pub const USD: &str = "usd";
const E18: u128 = 1_000_000_000_000_000_000;

/// A full-range LP share (a Smart LP vault, a pair token), priced from its two
/// tokens' feeds: USD per whole share = `k` x sqrt(P_a x P_b) / 1e18.
///
/// WHY this is the price: a full-range position of liquidity L holds L/sqrt(p)
/// of one token and L x sqrt(p) of the other; valued at the FEEDS' prices (not
/// the pool's, which a sandwich moves) it is worth 2 L sqrt(P_a P_b), whatever
/// the pool's price. Per share: 2 (L/S) sqrt(P_a P_b), so k = 1e18 x 2 L/S
/// (with the decimals folded in). `k` is the owner's, pinned in the template
/// from the chain (L and S, or the vault's own total value / supply): nothing
/// the keeper says reaches it. L/S only grows (fees compounded into L, a
/// withdrawal fee left behind), so a pinned k is never above the true one:
/// a redeem floor from it is never above fair, a deposit floor never below
/// (it asks for more shares, and refuses once the drift passes the slippage:
/// then the owner pins a fresh k).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct ShareValue {
    /// The market side this share is: the template's market.sell or market.buy.
    pub name: String,
    /// The pool's two tokens, as feed names (or `usd`).
    pub a: String,
    pub b: String,
    pub k: U128,
}

impl ShareValue {
    /// USD per whole share (1e18) at the feeds' prices `p_a`, `p_b` (1e18 each).
    pub fn usd_e18(&self, p_a: u128, p_b: u128) -> u128 {
        let root = (U256::from(p_a) * U256::from(p_b)).integer_sqrt();
        let r = U256::from(self.k.0) * root / U256::from(E18);
        if r > U256::from(u128::MAX) { u128::MAX } else { r.as_u128() }
    }
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
            require!(f.name != USD, "feed name: 'usd' is one dollar, never read (name the feed after its token)");
            check_id(&f.price_id);
            match &f.backup {
                Backup::Other { contract, price_id } => {
                    check_id(price_id);
                    require!(!(*contract == f.contract && price_id.eq_ignore_ascii_case(&f.price_id)),
                        format!("feed {}: a feed can't be its own backup (name another publisher, or single_source)", f.name));
                }
                Backup::SingleSource => self.emit("single_source_feed", &format!(r#""feed":"{}","note":"{SINGLE_SOURCE}""#, f.name)),
            }
        }
        self.vs.oracle = Some(config);
    }

    /// Refuse a market floor on any of `names` that has no second publisher.
    pub(crate) fn require_confirmed(&self, names: &[String]) {
        for name in names.iter().filter(|n| *n != USD) {
            if self.feed(name).backup == Backup::SingleSource {
                env::panic_str(&format!("refused: {name}: {SINGLE_SOURCE}"));
            }
        }
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
            if let Backup::Other { contract, price_id } = &f.backup {
                out.push(read(contract, "get_price_no_older_than", price_id));
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
                if matches!(f.backup, Backup::Other { .. }) {
                    // Named apart, so a refusal says which publisher had no fresh price.
                    let b = usd_e18(&next(&format!("{name} backup"))).unwrap_or_else(|| env::panic_str(&format!("refused: {name}: bad backup price")));
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

    /// The full vault's vectors (exact digits from Python fractions), now the brain's own.
    #[test]
    fn floors_from_prices_keep_every_digit() {
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

    /// LP shares priced from their tokens' feeds, with Robinhood Chain's numbers at block
    /// 109,995,929 (2026-11-09): Chainlink RHNVDA $179.02266357, AMZN $261.41373818, ETH/USD
    /// (Arbitrum) $2,104.32245252; the ETHNVDA pair token's positionLiquidity()
    /// 4,460,122,564,624,555,222 over totalSupply() 4,444,637,011,751,242,718; the audited
    /// Smart LP sdAMZN-FR-USDG's totalValueQuote() 8,923 (USDG units) over 1,000 shares (wei).
    /// Digits from Python integers.
    #[test]
    fn share_prices_follow_the_feeds_and_the_pinned_k_only() {
        let eth = usd_e18(&p(210_432_245_252, -8)).unwrap();
        let nvda = usd_e18(&p(17_902_266_357, -8)).unwrap();
        let amzn = usd_e18(&p(26_141_373_818, -8)).unwrap();
        // ETHNVDA: k = 1e18 x 2 L / S; a share is worth $1,231.83 (research read $1,231.18 a day earlier).
        let pair = ShareValue { name: "ethnvda".into(), a: "eth".into(), b: "nvda".into(), k: U128(2_006_968_196_877_436_792) };
        let v = pair.usd_e18(eth, nvda);
        assert_eq!(v, 1_231_829_618_763_315_536_020);
        // Zap 0.01 ETH in at 2% (pool fee and slippage): at least 0.016741 shares (the fork mints ~0.0170).
        let zap = Market { sell: "eth".into(), buy: "ethnvda".into(), dec_in: 18, dec_out: 18, slippage_bps: 200 };
        assert_eq!(market_min(&zap, 10_000_000_000_000_000, eth, v), 16_741_243_854_324_298);
        // Redeem 0.01 shares to ETH at 2%: at least 0.005737 ETH.
        let redeem = Market { sell: "ethnvda".into(), buy: "eth".into(), dec_in: 18, dec_out: 18, slippage_bps: 200 };
        assert_eq!(market_min(&redeem, 10_000_000_000_000_000, v, eth), 5_736_730_247_507_425);
        // Smart LP: $0.008923 per 1,000 share-wei = $8.923e12 per whole share, k pinned from it.
        let lp = ShareValue { name: "sdamzn".into(), a: "amzn".into(), b: USD.into(), k: U128(551_882_584_226_203_459_448_670_186_493) };
        let v = lp.usd_e18(amzn, E18);
        assert_eq!(v, 8_922_999_999_999_999_999_999_999_999_985, "the pinned k gives back the vault's own value");
        // Deposit 50 USDG at 1%: at least 5,547,461 shares (the fork minted 5,593,486).
        let dep = Market { sell: USD.into(), buy: "sdamzn".into(), dec_in: 6, dec_out: 18, slippage_bps: 100 };
        assert_eq!(market_min(&dep, 50_000_000, E18, v), 5_547_461);
        // Limits: no k or no price, no value; the square root never overflows at the biggest prices.
        assert_eq!(ShareValue { k: U128(0), ..pair.clone() }.usd_e18(eth, nvda), 0);
        assert_eq!(pair.usd_e18(0, nvda), 0);
        assert_eq!(ShareValue { k: U128(u128::MAX), ..pair.clone() }.usd_e18(u128::MAX, u128::MAX), u128::MAX);
        // Both tokens up 50%: the share is worth 50% more (it is linear in sqrt(P_a P_b)).
        assert_eq!(pair.usd_e18(eth * 3 / 2, nvda * 3 / 2) / 1_000_000, pair.usd_e18(eth, nvda) * 3 / 2 / 1_000_000);
    }

    const E18: u128 = 1_000_000_000_000_000_000;

    /// The rest of the full vault's oracle vectors (vault/src/oracle.rs, removed
    /// with the vault): kept here so the brain alone proves every digit.
    #[test]
    fn floors_per_input_limits_and_memecoin_sells_keep_every_digit() {
        let eth = usd_e18(&p(222_450_000_000, -8)).unwrap();
        let usdc = usd_e18(&p(100_010_000, -8)).unwrap();
        assert_eq!(eth, 2_224_500_000_000_000_000_000);
        let m = Market { sell: "eth".into(), buy: "usdc".into(), dec_in: 18, dec_out: 6, slippage_bps: 50 };
        // The same floor per unit of input, scaled 1e24, gives the same minimum.
        let per_in = market_per_in(&m, eth, usdc, 1_000_000_000_000_000_000_000_000);
        assert_eq!(crate::mul_div(100_000_000_000_000_000, per_in, 1_000_000_000_000_000_000_000_000) / 10_000, 22_131);
        // 1,000 USDC -> at least 1000 / 2224.5 x 0.995 = 0.4473 WETH.
        let back = Market { sell: "usdc".into(), buy: "eth".into(), dec_in: 6, dec_out: 18, slippage_bps: 50 };
        assert_eq!(market_min(&back, 1_000_000_000, usdc, eth) / 100_000_000_000_000, 4_473);
        // Limits: zero amount -> zero; full slippage -> zero; a zero price is no price.
        assert_eq!(market_min(&m, 0, eth, usdc), 0);
        assert_eq!(market_min(&Market { slippage_bps: 10_000, ..m.clone() }, 1, eth, usdc), 0);
        assert!(usd_e18(&p(0, -8)).is_none());
        // Positive exponents work (huge ones are refused, test above).
        assert_eq!(usd_e18(&p(5, 2)).unwrap(), 500 * E18);
        // A pump.fun coin at $5.1204e-8 keeps its digits; selling 1e9 coins at NEAR $2.00,
        // 1% slippage = 25.345980 NEAR exactly (Python fractions).
        let meme = usd_e18(&p(51_204, -12)).unwrap();
        assert_eq!(meme, 51_204_000_000);
        let near = usd_e18(&p(200_000_000, -8)).unwrap();
        let sell = Market { sell: "meme".into(), buy: "near".into(), dec_in: 6, dec_out: 24, slippage_bps: 100 };
        assert_eq!(market_min(&sell, 10u128.pow(15), meme, near), 25_345_980_000_000_000_000_000_000);
    }
}
