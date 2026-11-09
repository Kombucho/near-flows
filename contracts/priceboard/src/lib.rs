//! A price board: prices Pyth doesn't carry for us (fresh pump.fun memecoins;
//! NEAR and NVDA, which our Pyth key isn't entitled to; Chainlink's ETH, SOL and
//! USDC as the second publisher behind Pyth's), behind Pyth's own read interface
//! (get_price_no_older_than, same Price format), so a vault's oracle treats it as
//! just another Pyth-compatible contract: a feed's `contract` (or its backup's).
//! The price id is 32 bytes, hex: a coin's Solana mint, Pyth's own id for a
//! relayed Pyth price, or for a Chainlink feed its chain and proxy address
//! (chainlink::price_id).
//!
//! One board, one source of truth, chosen when it's created:
//! - Attested: prices signed (Ed25519) by k of n attesters, e.g. keepers that
//!   read Jupiter's price API. The board keeps the median. TRUSTS THE ATTESTERS:
//!   k of them colluding can post any price. Jupiter's API is off-chain and
//!   unsigned, so no contract can check it; this is the honest best for it.
//! - Wormhole: anyone posts a Wormhole Query response in which a quorum of
//!   Wormhole guardians attest the coin's pump.fun bonding curve account on
//!   Solana (read at the PDA the guardians derive themselves). The board checks
//!   the guardian signatures, reads the curve's reserves and prices the coin in
//!   its quote token, then in USD with Pyth. Trusts Wormhole's guardians (as Pyth
//!   already does) and the curve's spot state at that slot, not any keeper.
//! - Evm: anyone posts a Wormhole Query response in which a quorum of guardians
//!   ran latestRoundData() and decimals() on a Chainlink feed the owner
//!   registered (chain + proxy address) and signed the answers with the block's
//!   time. The board checks the signatures, the chain, the address, the call
//!   data, the block's age and the feed's own staleness. Trusts Wormhole's
//!   guardians and Chainlink's feed (its node operators, and the multisig that
//!   can repoint its proxy), not any keeper.
//! - PythCore: anyone posts a Pyth Core update exactly as Hermes serves it (an
//!   accumulator update: a signed Merkle root plus each price's proof). The
//!   board checks the signatures (a majority of Pyth's own signer set, pinned
//!   at deploy and rotated only by that set's own upgrade VAAs), the emitter
//!   (Pythnet), each price's Merkle proof, that the feed is listed, and the
//!   price's age. Trusts Pyth's signers (its 5 routers since Aug 2026), as
//!   Pyth's own receivers do, not any keeper. WHY here and not Pyth's NEAR
//!   receiver: that one still wants the pre-upgrade emitter and refuses every
//!   update since (pythcore.rs).
//!
//! Whatever the source, the board keeps one thing per feed: its latest price. It
//! keeps no average of its own. WHY: prices are posted on demand (just before a
//! step), so an average of them is only as honest as the poster's timing, and a
//! vault's "price and backup agree" check needs a second, independent publisher
//! (Chainlink for a Pyth feed, Chainlink on another chain for Chainlink), read at
//! the same moment, not the first publisher's own past.

pub mod chainlink;
pub mod pump;
pub mod pythcore;
pub mod wormhole;

use near_sdk::json_types::{I64, U64};
use near_sdk::store::LookupMap;
use near_sdk::{env, near, require, AccountId, CurveType, Gas, NearToken, PanicOnDefault, Promise, PromiseError, PublicKey};

/// What attesters sign: domain tag, then feed id, price, expo, publish time (big-endian).
/// Nothing about a board or vault, so one signature serves every board.
pub const DOMAIN: &[u8] = b"NEARFLOWS_PRICE_V1";
/// A price may be at most this far in the future (clocks differ a little).
const SKEW_S: i64 = 30;
/// Posting a coin the board has never seen pays for its storage (about 200 bytes).
pub const NEW_FEED_DEPOSIT: NearToken = NearToken::from_millinear(5);
const GAS_READ: Gas = Gas::from_tgas(5);
const GAS_CALLBACK: Gas = Gas::from_tgas(20);

/// Pyth's price format: `price` x 10^`expo`, +-`conf`, published at `publish_time` (unix s).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    pub price: I64,
    pub conf: U64,
    pub expo: i32,
    pub publish_time: i64,
}

/// A curve's quote token: its mint (hex; all zero = SOL), its Pyth USD feed and decimals.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct Quote {
    pub mint: String,
    pub pyth_id: String,
    pub decimals: u8,
}

#[near(serializers = [borsh, json])]
#[serde(tag = "kind", rename_all = "snake_case")]
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// k of these Ed25519 keys must sign each price; their reports may be at most `max_skew_s` apart.
    Attested { attesters: Vec<PublicKey>, k: u8, max_skew_s: u64 },
    /// Wormhole Queries of pump.fun curves. Guardians: 20-byte addresses (hex) of set `guardian_set`;
    /// later sets arrive as signed upgrade VAAs. Quote prices from `pyth`, no older than `quote_age_s`.
    Wormhole { guardian_set: u32, guardians: Vec<String>, pyth: AccountId, quotes: Vec<Quote>, quote_age_s: u64 },
    /// Wormhole Queries (eth_call) of these Chainlink feeds. Guardians as for `Wormhole`.
    /// A response's block may be at most `max_block_age_s` old when posted.
    Evm { guardian_set: u32, guardians: Vec<String>, feeds: Vec<EvmFeed>, max_block_age_s: u64 },
    /// Pyth Core updates (Hermes's accumulator updates). Guardians: Pyth's own signer set
    /// `guardian_set` (20-byte addresses, hex), a majority of them needed; later sets
    /// arrive as upgrade VAAs that set signed. Roots must come from `emitter_chain` /
    /// `emitter` (32 bytes, hex). Only `feeds` (Pyth ids, hex) are kept, each no older
    /// than `max_age_s` when posted.
    PythCore { guardian_set: u32, guardians: Vec<String>, emitter_chain: u16, emitter: String, feeds: Vec<String>, max_age_s: u64 },
}

/// A Chainlink feed the board takes prices from (its price id: chainlink::price_id).
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug, PartialEq)]
pub struct EvmFeed {
    /// Wormhole chain id: 2 Ethereum, 23 Arbitrum, 24 Optimism, 72 Robinhood Chain...
    pub chain: u16,
    /// The feed's proxy (0x + 40 hex, lowercase): the address Chainlink's docs list.
    pub address: String,
    /// The feed's deviation threshold, e.g. 50 (0.5%). WHY it's the uncertainty
    /// (conf): Chainlink only writes a new answer once the market moves this
    /// far, so between updates the true price can be anywhere within it.
    pub deviation_bps: u32,
    /// The feed's heartbeat: an answer older than this at the block means the
    /// feed stopped updating (or the market is closed), and is refused.
    pub heartbeat_s: u64,
}

/// One attester's signed price.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Report {
    pub attester: PublicKey,
    pub price: I64,
    pub expo: i32,
    pub publish_time: i64,
    /// 64 bytes, hex.
    pub signature: String,
}

/// A curve read from a verified query, waiting for its quote token's USD price.
#[near(serializers = [json])]
#[derive(Clone, Debug)]
pub struct Pending {
    pub feed: String,
    pub virtual_token: U64,
    pub virtual_quote: U64,
    pub time: i64,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Board {
    /// May change attesters or quotes; None = nobody, for good.
    owner: Option<AccountId>,
    source: Source,
    prices: LookupMap<String, Price>,
}

#[near]
impl Board {
    #[init]
    pub fn new(owner: Option<AccountId>, source: Source) -> Self {
        check_source(&source);
        Self { owner, source, prices: LookupMap::new(b"p") }
    }

    // ---------- Pyth's read interface ----------

    pub fn get_price_no_older_than(&self, price_id: String, age: u64) -> Option<Price> {
        self.prices.get(&price_id.to_lowercase()).cloned().filter(|p| fresh(p, age))
    }

    /// The latest price, however old (check `publish_time`).
    pub fn get_price_unsafe(&self, price_id: String) -> Option<Price> {
        self.prices.get(&price_id.to_lowercase()).cloned()
    }

    pub fn get_source(&self) -> Source {
        self.source.clone()
    }

    pub fn get_owner(&self) -> Option<AccountId> {
        self.owner.clone()
    }

    /// The exact bytes an attester signs (hex), for signers to check themselves against.
    pub fn attestation(&self, feed: String, price: I64, expo: i32, publish_time: i64) -> String {
        hex::encode(attestation_bytes(&feed_id(&feed), price.0, expo, publish_time))
    }

    // ---------- Attested ----------

    /// Anyone: post k (or more) attesters' signed prices for one coin. The board
    /// keeps the median, the spread between attesters as its uncertainty (conf),
    /// and the oldest report's time (a price is only as fresh as its stalest vote).
    pub fn push(&mut self, feed: String, reports: Vec<Report>) -> Price {
        let Source::Attested { attesters, k, max_skew_s } = &self.source else { env::panic_str("this board takes Wormhole queries, not attestations") };
        let id = feed_id(&feed);
        let mut seen: Vec<&PublicKey> = Vec::new();
        for r in &reports {
            require!(attesters.contains(&r.attester), format!("refused: {:?} is not an attester", String::from(&r.attester)));
            require!(!seen.contains(&&r.attester), "refused: one attester, one vote");
            seen.push(&r.attester);
            let sig: [u8; 64] = hex::decode(&r.signature).ok().and_then(|s| s.try_into().ok()).unwrap_or_else(|| env::panic_str("signature: 64 bytes, hex"));
            let pk: &[u8; 32] = r.attester.as_bytes()[1..].try_into().unwrap();
            require!(env::ed25519_verify(&sig, attestation_bytes(&id, r.price.0, r.expo, r.publish_time), pk), "refused: bad attester signature");
        }
        require!(reports.len() >= *k as usize, format!("refused: {} of {} attesters signed, {} needed", reports.len(), attesters.len(), k));
        let expo = reports[0].expo;
        require!(reports.iter().all(|r| r.expo == expo), "refused: reports use different exponents");
        let (oldest, newest) = (reports.iter().map(|r| r.publish_time).min().unwrap(), reports.iter().map(|r| r.publish_time).max().unwrap());
        require!((newest - oldest) as u64 <= *max_skew_s, "refused: reports too far apart in time");
        require!(newest <= now() + SKEW_S, "refused: price from the future");
        let mut ps: Vec<i64> = reports.iter().map(|r| r.price.0).collect();
        ps.sort_unstable();
        require!(ps[0] > 0, "refused: a price must be positive");
        let n = ps.len();
        let median = if n % 2 == 1 { ps[n / 2] } else { ((ps[n / 2 - 1] as i128 + ps[n / 2] as i128) / 2) as i64 };
        let conf = ((ps[n - 1] - ps[0]) / 2) as u64;
        let p = Price { price: I64(median), conf: U64(conf), expo, publish_time: oldest };
        require!(self.store(&hex::encode(id), p.clone()), "refused: not newer than the board's price");
        p
    }

    /// Owner: change the attesters or the threshold.
    pub fn set_attesters(&mut self, attesters: Vec<PublicKey>, k: u8, max_skew_s: u64) {
        self.assert_owner();
        require!(matches!(self.source, Source::Attested { .. }), "not an attested board");
        let s = Source::Attested { attesters, k, max_skew_s };
        check_source(&s);
        self.source = s;
    }

    // ---------- Wormhole ----------

    /// Anyone: post a Wormhole Query response (hex, as the query proxy returns
    /// it) with its guardian signatures. Each account must be a pump.fun
    /// bonding curve at PDA ["bonding-curve", mint], read at finalized
    /// commitment. Prices are written once the quote token's Pyth price is in.
    #[payable]
    pub fn push_query(&mut self, response: String, signatures: Vec<String>) -> Promise {
        let Source::Wormhole { guardian_set: _, guardians, pyth, quotes, quote_age_s } = &self.source else { env::panic_str("this board doesn't take pump.fun curve queries") };
        let bytes = hex::decode(&response).unwrap_or_else(|_| env::panic_str("response: hex"));
        let sigs: Vec<_> = signatures.iter().map(|s| wormhole::proxy_sig(s).unwrap_or_else(|e| env::panic_str(e))).collect();
        let set: Vec<[u8; 20]> = guardians.iter().map(|g| addr(g)).collect();
        wormhole::verify_quorum(&wormhole::response_digest(&bytes), &sigs, &set).unwrap_or_else(|e| env::panic_str(&format!("refused: {e}")));

        let per = wormhole::parse_response(&bytes).unwrap_or_else(|e| env::panic_str(&format!("refused: {e}")));
        require!(per.len() == 1, "refused: one Solana query per response");
        let q = wormhole::parse_sol_pda(&per[0]).unwrap_or_else(|e| env::panic_str(&format!("refused: {e}")));
        require!(q.commitment == b"finalized", "refused: read at finalized commitment only");
        require!(q.slice == (0, 0), "refused: the whole account, not a slice");
        let time = (q.block_time_us / 1_000_000) as i64;
        require!(time <= now() + SKEW_S, "refused: Solana block from the future");

        let mut quote: Option<&Quote> = None;
        let mut pending = Vec::with_capacity(q.accounts.len());
        let mut new_feeds = 0u128;
        for a in &q.accounts {
            // The guardians derived the address from (program, seeds); the
            // account must also BE the program's (else anyone's look-alike data).
            require!(a.program == pump::PROGRAM && a.owner == pump::PROGRAM, "refused: not a pump.fun account");
            require!(a.seeds.len() == 2 && a.seeds[0] == pump::SEED && a.seeds[1].len() == 32, "refused: not a bonding curve PDA");
            let c = pump::decode(&a.data).unwrap_or_else(|e| env::panic_str(&format!("refused: {e}")));
            require!(!c.complete, "refused: curve complete (the coin trades on its AMM pool now)");
            let qm = hex::encode(c.quote_mint);
            let this = quotes.iter().find(|x| x.mint == qm).unwrap_or_else(|| env::panic_str(&format!("refused: unsupported quote mint {qm}")));
            require!(quote.is_none_or(|x| x == this), "refused: one quote token per response");
            quote = Some(this);
            let feed = hex::encode(&a.seeds[1]);
            if !self.prices.contains_key(&feed) {
                new_feeds += 1;
            }
            pending.push(Pending { feed, virtual_token: U64(c.virtual_token), virtual_quote: U64(c.virtual_quote), time });
        }
        let quote = quote.unwrap_or_else(|| env::panic_str("refused: no accounts")).clone();
        require!(env::attached_deposit().as_yoctonear() >= NEW_FEED_DEPOSIT.as_yoctonear() * new_feeds, format!("attach {} yoctoNEAR per new coin ({new_feeds} new)", NEW_FEED_DEPOSIT.as_yoctonear()));

        let args = format!(r#"{{"price_id":"{}","age":{}}}"#, quote.pyth_id, quote_age_s);
        Promise::new(pyth.clone())
            .function_call("get_price_no_older_than", args.into_bytes(), NearToken::from_yoctonear(0), GAS_READ)
            .then(Self::ext(env::current_account_id()).with_static_gas(GAS_CALLBACK).on_quote(pending, quote.decimals))
    }

    /// The quote token's USD price is in: price each curve, store what's newer.
    #[private]
    pub fn on_quote(&mut self, pending: Vec<Pending>, decimals: u8, #[callback_result] q: Result<Option<Price>, PromiseError>) -> Vec<Price> {
        let q = q.ok().flatten().unwrap_or_else(|| env::panic_str("refused: no fresh Pyth price for the quote token"));
        let mut out = Vec::new();
        for p in pending {
            let c = pump::Curve { virtual_token: p.virtual_token.0, virtual_quote: p.virtual_quote.0, complete: false, quote_mint: [0; 32] };
            let usd = pump::usd_e12(&c, q.price.0, q.expo, decimals as u32).unwrap_or_else(|| env::panic_str("refused: bad curve price"));
            require!(usd > 0, "refused: price rounds to zero");
            // The curve is exact; the uncertainty is the quote token's, in proportion.
            let conf = (usd as u128 * q.conf.0 as u128 / q.price.0 as u128) as u64;
            // Only as fresh as the older of the two readings.
            let price = Price { price: I64(usd), conf: U64(conf), expo: pump::EXPO, publish_time: p.time.min(q.publish_time) };
            if self.store(&p.feed, price.clone()) {
                env::log_str(&format!(r#"EVENT_JSON:{{"standard":"priceboard","version":"1","event":"price","data":{{"feed":"{}","price":"{}","expo":{},"publish_time":{}}}}}"#, p.feed, usd, pump::EXPO, price.publish_time));
                out.push(price);
            }
        }
        out
    }

    // ---------- Evm (Chainlink via Wormhole Queries) ----------

    /// Anyone: post a Wormhole Query response (hex, as the query proxy returns
    /// it) with its guardian signatures. Every per-chain query must be an
    /// eth_call that reads, for each feed, latestRoundData() then decimals()
    /// on a registered feed's proxy. Each feed's price: answer x 10^-decimals,
    /// published at the block's time, uncertain by the feed's deviation
    /// threshold. Returns what was stored (at least one, or it's refused).
    pub fn push_evm(&mut self, response: String, signatures: Vec<String>) -> Vec<Price> {
        let Source::Evm { guardians, feeds, max_block_age_s, .. } = &self.source else { env::panic_str("this board doesn't take Chainlink (eth_call) queries") };
        let bytes = hex::decode(&response).unwrap_or_else(|_| env::panic_str("response: hex"));
        let sigs: Vec<_> = signatures.iter().map(|s| wormhole::proxy_sig(s).unwrap_or_else(|e| env::panic_str(e))).collect();
        let set: Vec<[u8; 20]> = guardians.iter().map(|g| addr(g)).collect();
        let refuse = |e: &str| -> ! { env::panic_str(&format!("refused: {e}")) };
        wormhole::verify_quorum(&wormhole::response_digest(&bytes), &sigs, &set).unwrap_or_else(|e| refuse(e));

        let mut read: Vec<(String, Price, String)> = Vec::new();
        for pc in wormhole::parse_response(&bytes).unwrap_or_else(|e| refuse(e)) {
            let q = wormhole::parse_eth_call(&pc).unwrap_or_else(|e| refuse(e));
            let time = (q.block_time_us / 1_000_000) as i64;
            require!(time <= now() + SKEW_S, "refused: block from the future");
            // WHY a bound on the block's age: the requester picks the block, so
            // without one an old block could be posted as if it were fresh.
            require!(now() - time <= *max_block_age_s as i64, format!("refused: block {} s old, at most {max_block_age_s}", now() - time));
            require!(!q.calls.is_empty() && q.calls.len() % 2 == 0, "refused: each feed is read with latestRoundData() then decimals()");
            for pair in q.calls.chunks(2) {
                let (round, dec) = (&pair[0], &pair[1]);
                require!(round.data == chainlink::LATEST_ROUND_DATA && dec.data == chainlink::DECIMALS && dec.to == round.to,
                    "refused: each feed is read with latestRoundData() then decimals()");
                let a = format!("0x{}", hex::encode(round.to));
                let f = feeds.iter().find(|f| f.chain == q.chain && f.address == a)
                    .unwrap_or_else(|| env::panic_str(&format!("refused: {a} on chain {} is not a registered feed", q.chain)));
                let r = chainlink::decode_round(&round.result).unwrap_or_else(|e| refuse(e));
                let d = chainlink::decode_decimals(&dec.result).unwrap_or_else(|e| refuse(e));
                let updated = r.updated_at as i64;
                require!(updated <= time, "refused: answer updated after its block");
                require!(time - updated <= f.heartbeat_s as i64,
                    format!("refused: {a} on chain {}: answer {} s old at the block, heartbeat {} s (feed stale or market closed)", q.chain, time - updated, f.heartbeat_s));
                let conf = (r.answer as u128 * f.deviation_bps as u128 / 10_000) as u64;
                // Published at the block's time, not updatedAt: at that block the
                // feed vouched that the price is within its deviation of the answer.
                let price = Price { price: I64(r.answer), conf: U64(conf), expo: -(d as i32), publish_time: time };
                let src = format!(r#""chain":{},"address":"{a}","block":{},"round_id":"{}","updated_at":{}"#, q.chain, q.block_num, r.round_id, r.updated_at);
                read.push((hex::encode(chainlink::price_id(f.chain, &round.to)), price, src));
            }
        }
        let mut out = Vec::new();
        for (feed, price, src) in read {
            if self.store(&feed, price.clone()) {
                env::log_str(&format!(r#"EVENT_JSON:{{"standard":"priceboard","version":"1","event":"price","data":{{"feed":"{feed}","price":"{}","expo":{},"publish_time":{},{src}}}}}"#,
                    price.price.0, price.expo, price.publish_time));
                out.push(price);
            }
        }
        require!(!out.is_empty(), "refused: not newer than the board's prices");
        out
    }

    /// Owner: the Chainlink feeds this board takes.
    pub fn set_evm_feeds(&mut self, feeds: Vec<EvmFeed>) {
        self.assert_owner();
        let Source::Evm { feeds: f, .. } = &mut self.source else { env::panic_str("not an Evm board") };
        *f = feeds;
        check_source(&self.source);
    }

    /// A Chainlink feed's price id (hex): Wormhole chain id, 10 zero bytes, proxy address.
    pub fn evm_price_id(&self, chain: u16, address: String) -> String {
        hex::encode(chainlink::price_id(chain, &addr(&address)))
    }

    // ---------- PythCore ----------

    /// Anyone: post a Pyth Core update (hex, exactly as Hermes serves it with
    /// encoding=hex). The board checks a majority of its Pyth signer set signed
    /// the root, the emitter, and each price's Merkle proof under that root;
    /// each price must be a listed feed, positive, and at most `max_age_s` old.
    /// Stores what's newer than the board's price; refused if nothing is.
    pub fn post_update(&mut self, data: String) -> Vec<Price> {
        let Source::PythCore { guardian_set, guardians, emitter_chain, emitter, feeds, max_age_s } = &self.source else {
            env::panic_str("this board doesn't take Pyth Core updates")
        };
        let refuse = |e: &str| -> ! { env::panic_str(&format!("refused: {e}")) };
        let bytes = hex::decode(data.trim_start_matches("0x")).unwrap_or_else(|_| env::panic_str("data: hex"));
        let set: Vec<[u8; 20]> = guardians.iter().map(|g| addr(g)).collect();
        let v = pythcore::verify(&bytes, *guardian_set, &set, *emitter_chain, &feed_id(emitter)).unwrap_or_else(|e| refuse(e));
        let mut read = Vec::with_capacity(v.prices.len());
        for m in &v.prices {
            let id = hex::encode(m.id);
            require!(feeds.contains(&id), format!("refused: {id} is not a listed feed"));
            require!(m.price > 0, format!("refused: {id}: a price must be positive"));
            require!(m.publish_time <= now() + SKEW_S, format!("refused: {id}: price from the future"));
            let age = now() - m.publish_time;
            require!(age <= *max_age_s as i64, format!("refused: {id}: price {age} s old, at most {max_age_s}"));
            read.push((id, Price { price: I64(m.price), conf: U64(m.conf), expo: m.expo, publish_time: m.publish_time }));
        }
        let mut out = Vec::new();
        for (feed, price) in read {
            if self.store(&feed, price.clone()) {
                env::log_str(&format!(r#"EVENT_JSON:{{"standard":"priceboard","version":"1","event":"price","data":{{"feed":"{feed}","price":"{}","conf":"{}","expo":{},"publish_time":{},"pythnet_slot":{},"sequence":{}}}}}"#,
                    price.price.0, price.conf.0, price.expo, price.publish_time, v.slot, v.sequence));
                out.push(price);
            }
        }
        require!(!out.is_empty(), "refused: not newer than the board's prices");
        out
    }

    /// Owner: the Pyth feeds this board keeps (Pyth ids, hex).
    pub fn set_pyth_feeds(&mut self, feeds: Vec<String>) {
        self.assert_owner();
        let Source::PythCore { feeds: f, .. } = &mut self.source else { env::panic_str("not a PythCore board") };
        *f = feeds;
        check_source(&self.source);
    }

    /// Anyone: follow the signer set's rotation with the upgrade VAA the
    /// current set signed (hex): Wormhole's guardians on a Wormhole or Evm
    /// board, Pyth's signers on a PythCore one. No owner involved.
    pub fn upgrade_guardians(&mut self, vaa: String) -> u32 {
        let (guardian_set, guardians, need): (_, _, fn(usize) -> usize) = match &mut self.source {
            Source::Wormhole { guardian_set, guardians, .. } | Source::Evm { guardian_set, guardians, .. } => (guardian_set, guardians, wormhole::quorum),
            Source::PythCore { guardian_set, guardians, .. } => (guardian_set, guardians, pythcore::quorum),
            Source::Attested { .. } => env::panic_str("not a Wormhole board"),
        };
        let bytes = hex::decode(&vaa).unwrap_or_else(|_| env::panic_str("vaa: hex"));
        let set: Vec<[u8; 20]> = guardians.iter().map(|g| addr(g)).collect();
        let up = wormhole::guardian_upgrade_with(&bytes, *guardian_set, &set, need).unwrap_or_else(|e| env::panic_str(&format!("refused: {e}")));
        *guardian_set = up.index;
        *guardians = up.guardians.iter().map(hex::encode).collect();
        up.index
    }

    /// Owner: the quote tokens curves may be priced in, and where their USD prices come from.
    pub fn set_quotes(&mut self, quotes: Vec<Quote>) {
        self.assert_owner();
        let Source::Wormhole { quotes: q, .. } = &mut self.source else { env::panic_str("not a Wormhole board") };
        *q = quotes;
        check_source(&self.source);
    }

    /// Owner: hand over the board, or (None) give it up for good.
    pub fn set_owner(&mut self, owner: Option<AccountId>) {
        self.assert_owner();
        self.owner = owner;
    }
}

impl Board {
    fn assert_owner(&self) {
        require!(self.owner.as_ref() == Some(&env::predecessor_account_id()), "owner only");
    }

    /// Keep `p` if it's newer than what the board has.
    fn store(&mut self, feed: &str, p: Price) -> bool {
        if self.prices.get(feed).is_some_and(|e| p.publish_time <= e.publish_time) {
            return false;
        }
        self.prices.insert(feed.to_string(), p);
        true
    }
}

pub fn attestation_bytes(feed: &[u8; 32], price: i64, expo: i32, publish_time: i64) -> Vec<u8> {
    let mut m = DOMAIN.to_vec();
    m.extend_from_slice(feed);
    m.extend_from_slice(&price.to_be_bytes());
    m.extend_from_slice(&expo.to_be_bytes());
    m.extend_from_slice(&publish_time.to_be_bytes());
    m
}

fn feed_id(feed: &str) -> [u8; 32] {
    hex::decode(feed).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| env::panic_str("feed: 32 bytes, hex (a Solana mint, or chainlink::price_id)"))
}

fn addr(g: &str) -> [u8; 20] {
    hex::decode(g.trim_start_matches("0x")).ok().and_then(|b| b.try_into().ok()).unwrap_or_else(|| env::panic_str("guardian: 20 bytes, hex"))
}

fn now() -> i64 {
    (env::block_timestamp() / 1_000_000_000) as i64
}

/// Like Pyth: no price if it's more than `age` seconds away from now.
fn fresh(p: &Price, age: u64) -> bool {
    (now() as u64).abs_diff(p.publish_time as u64) <= age
}

fn check_source(s: &Source) {
    match s {
        Source::Attested { attesters, k, max_skew_s } => {
            require!(*k >= 1 && (*k as usize) <= attesters.len(), "need 1 <= k <= attesters");
            require!(attesters.len() <= 16 && attesters.iter().all(|a| a.curve_type() == CurveType::ED25519), "at most 16 attesters, Ed25519 keys");
            require!(*max_skew_s <= 600, "max_skew_s <= 600");
        }
        Source::Evm { guardians, feeds, max_block_age_s, .. } => {
            check_guardians(guardians);
            require!(*max_block_age_s >= 1 && *max_block_age_s <= 600, "max_block_age_s: 1 s to 10 min");
            require!(feeds.len() <= 32, "at most 32 feeds");
            for (i, f) in feeds.iter().enumerate() {
                require!(f.chain != 0 && f.chain != wormhole::SOLANA, "feed chain: an EVM chain's Wormhole id");
                require!(f.address.len() == 42 && f.address.starts_with("0x") && f.address == f.address.to_lowercase(), "feed address: 0x + 40 lowercase hex");
                addr(&f.address);
                require!(f.deviation_bps >= 1 && f.deviation_bps <= 1_000, "deviation_bps: 1 to 1000");
                require!(f.heartbeat_s >= 1 && f.heartbeat_s <= 7 * 86_400, "heartbeat_s: 1 s to 7 days");
                require!(!feeds[..i].iter().any(|g| g.chain == f.chain && g.address == f.address), "feed listed twice");
            }
        }
        Source::Wormhole { guardians, quotes, quote_age_s, .. } => {
            check_guardians(guardians);
            require!(*quote_age_s >= 1 && *quote_age_s <= 3_600, "quote_age_s: 1 s to 1 h");
            for q in quotes {
                feed_id(&q.mint);
                require!(q.pyth_id.len() == 64 && hex::decode(&q.pyth_id).is_ok() && q.mint == q.mint.to_lowercase(), "quote: lowercase hex mint, 32-byte Pyth id");
            }
        }
        Source::PythCore { guardians, emitter, feeds, max_age_s, .. } => {
            check_guardians(guardians);
            require!(emitter.len() == 64 && *emitter == emitter.to_lowercase() && hex::decode(emitter).is_ok(), "emitter: 32 bytes, lowercase hex");
            require!(*max_age_s >= 1 && *max_age_s <= 3_600, "max_age_s: 1 s to 1 h");
            require!(feeds.len() <= 32, "at most 32 feeds");
            for (i, f) in feeds.iter().enumerate() {
                require!(f.len() == 64 && *f == f.to_lowercase() && hex::decode(f).is_ok(), "feed: a Pyth id, 32 bytes, lowercase hex");
                require!(!feeds[..i].contains(f), "feed listed twice");
            }
        }
    }
}

fn check_guardians(guardians: &[String]) {
    require!(!guardians.is_empty() && guardians.len() <= 64, "1 to 64 guardians");
    guardians.iter().for_each(|g| {
        addr(g);
    });
}

#[cfg(test)]
mod tests;
