// Chainlink price feeds on other chains, for the price board (contracts/priceboard):
// which feeds, how to read them with a plain eth_call, how to decode the answer,
// the board's price id for each, and the Wormhole Query that asks the guardians
// to make the same read. Pure functions, tested in chainlink.test.ts.
//
// WHY Chainlink on other chains: our Pyth key covers ETH, BTC, SOL and USDC but
// not NEAR/USD or NVDA ("Not entitled"). Chainlink publishes both on chains that
// Wormhole Queries can read, so an Evm board can check them without trusting us.
// And every feed needs a backup from a DIFFERENT publisher (a vault checks the two
// agree, read in the same round): Chainlink on Arbitrum backs Pyth's ETH, SOL and
// USDC, and Chainlink's NEAR/USD on Optimism backs the one on Arbitrum. NVDA has
// no independent second source; a vault names it single_source.

/** keccak256("latestRoundData()")[..4] and keccak256("decimals()")[..4]. */
export const LATEST_ROUND_DATA = "feaf968c";
export const DECIMALS = "313ce567";

export type Feed = {
  /** What the feed's description() says, checked on every read. */
  description: string;
  /** Wormhole chain id (the board's and the price id's), and the EVM chain id (checked against the RPC). */
  chain: number;
  evmChainId: number;
  /** The feed's proxy, as Chainlink's docs list it. */
  address: string;
  /** A public RPC (no key); `rpcEnv` overrides it. */
  rpc: string;
  rpcEnv: string;
  /** From Chainlink's reference data directory: a new answer every `heartbeatS`, or on a `deviationBps` move. */
  heartbeatS: number;
  deviationBps: number;
};

// Verified by eth_call of description(), decimals() and latestRoundData() against
// these RPCs: near, near-op, nvda on 2026-11-06, the -arb backups on 2026-11-08
// (scripts/DEPLOY-RELAY.md has the values read). Heartbeat and deviation from
// Chainlink's reference data directory (reference-data-directory.vercel.app/
// feeds-ethereum-mainnet-arbitrum-1.json), the primary (non-SVR) proxy of each pair.
export const FEEDS: Record<string, Feed> = {
  // NEAR/USD on Arbitrum: Chainlink's NEAR/USD feed, 8 decimals.
  near: { description: "NEAR / USD", chain: 23, evmChainId: 42161, address: "0xbf5c3fb2633e924598a46b9d07a174a9dbcf57c0",
    rpc: "https://arb1.arbitrum.io/rpc", rpcEnv: "ARBITRUM_RPC", heartbeatS: 86_400, deviationBps: 50 },
  // The same pair on Optimism: a second, separately updated feed, for a vault's Backup::Other.
  "near-op": { description: "NEAR / USD", chain: 24, evmChainId: 10, address: "0xca6fa4b8cb365c02cd3ba70544efffe78f63ac82",
    rpc: "https://mainnet.optimism.io", rpcEnv: "OPTIMISM_RPC", heartbeatS: 86_400, deviationBps: 50 },
  // Backups for Pyth's ETH, SOL and USDC (the board serves Pyth's price under Pyth's id, these under theirs).
  "eth-arb": { description: "ETH / USD", chain: 23, evmChainId: 42161, address: "0x639fe6ab55c921f74e7fac1ee960c0b6293ba612",
    rpc: "https://arb1.arbitrum.io/rpc", rpcEnv: "ARBITRUM_RPC", heartbeatS: 1_755, deviationBps: 5 },
  "sol-arb": { description: "SOL / USD", chain: 23, evmChainId: 42161, address: "0x24cea4b8ce57cda5058b924b9b9987992450590c",
    rpc: "https://arb1.arbitrum.io/rpc", rpcEnv: "ARBITRUM_RPC", heartbeatS: 86_400, deviationBps: 10 },
  "usdc-arb": { description: "USDC / USD", chain: 23, evmChainId: 42161, address: "0x50834f3163758fcc1df9973b6e91f0f0f0434ad3",
    rpc: "https://arb1.arbitrum.io/rpc", rpcEnv: "ARBITRUM_RPC", heartbeatS: 255, deviationBps: 10 },
  // Robinhood's NVDA token on Robinhood Chain (Chainlink is the chain's official oracle).
  // Its answer is the TOKEN's price: NVDA's market price x the token's uiMultiplier
  // (dividends reinvested, splits), which is what a vault holding the token trades.
  nvda: { description: "RHNVDA / USD", chain: 72, evmChainId: 4663, address: "0x379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15",
    rpc: "https://rpc.mainnet.chain.robinhood.com", rpcEnv: "ROBINHOOD_RPC", heartbeatS: 86_400, deviationBps: 50 },
};

/** The board's price id: Wormhole chain id (2 bytes), 10 zero bytes, the proxy (20 bytes). Same as the contract's. */
export function priceId(chain: number, address: string): string {
  const a = address.toLowerCase().replace(/^0x/, "");
  if (!/^[0-9a-f]{40}$/.test(a)) throw new Error(`not an EVM address: ${address}`);
  if (!Number.isInteger(chain) || chain < 1 || chain > 0xffff) throw new Error(`not a Wormhole chain id: ${chain}`);
  return chain.toString(16).padStart(4, "0") + "00".repeat(10) + a;
}

export type Round = { roundId: bigint; answer: bigint; startedAt: bigint; updatedAt: bigint; answeredInRound: bigint };

const words = (hex: string, n: number): bigint[] => {
  const h = hex.replace(/^0x/, "");
  if (h.length !== n * 64) throw new Error(`expected ${n} words, got ${h.length / 2} bytes`);
  return Array.from({ length: n }, (_, i) => BigInt("0x" + h.slice(i * 64, (i + 1) * 64)));
};

/** latestRoundData()'s five words. The answer is an int256: refused if negative or zero. */
export function decodeRound(hex: string): Round {
  const [roundId, raw, startedAt, updatedAt, answeredInRound] = words(hex, 5);
  const answer = raw >= 1n << 255n ? raw - (1n << 256n) : raw;
  if (answer <= 0n) throw new Error(`answer ${answer} is not a price`);
  return { roundId, answer, startedAt, updatedAt, answeredInRound };
}

export function decodeDecimals(hex: string): number {
  const [d] = words(hex, 1);
  if (d > 18n) throw new Error(`decimals ${d}`);
  return Number(d);
}

/** An ABI string return (description()). */
export function decodeString(hex: string): string {
  const b = Buffer.from(hex.replace(/^0x/, ""), "hex");
  const len = Number(BigInt("0x" + b.subarray(32, 64).toString("hex")));
  return b.subarray(64, 64 + len).toString("utf8");
}

/** Phase (which aggregator behind the proxy) and the round within it: Chainlink's roundId = phase << 64 | round. */
export function phaseRound(roundId: bigint): { phase: bigint; round: bigint } {
  return { phase: roundId >> 64n, round: roundId & ((1n << 64n) - 1n) };
}

/**
 * What the board will accept, decided here first so we never sign what it would
 * refuse: the answer must be at most `heartbeatS` old at the block. Returns
 * why not, or null if fine.
 */
export function stale(r: Round, blockTime: number, heartbeatS: number): string | null {
  const age = blockTime - Number(r.updatedAt);
  if (age < 0) return `answer updated ${-age} s after its block`;
  if (age > heartbeatS) return `answer ${age} s old at the block, heartbeat ${heartbeatS} s (feed stale or market closed)`;
  return null;
}

const u16 = (n: number) => { const b = Buffer.alloc(2); b.writeUInt16BE(n); return b; };
const u32 = (n: number) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const v = (b: Uint8Array) => Buffer.concat([u32(b.length), b]);

export type ChainCalls = { chain: number; block: string; calls: { to: string; data: string }[] };

/**
 * A Wormhole Query request of eth_calls (type 1): per chain, a block ("0x..."
 * number) and calls. The layout the guardians and the board parse
 * (wormhole-solidity-sdk QueryRequest), checked against a real request in the tests.
 */
export function ethCallRequest(perChain: ChainCalls[], nonce = Math.floor(Math.random() * 2 ** 32)): Uint8Array {
  if (!perChain.length || perChain.length > 255) throw new Error("1 to 255 chains per query");
  const qs = perChain.map((q) => {
    const calls = q.calls.map((c) => Buffer.concat([Buffer.from(c.to.replace(/^0x/, ""), "hex"), v(Buffer.from(c.data.replace(/^0x/, ""), "hex"))]));
    const body = Buffer.concat([v(Buffer.from(q.block)), Buffer.from([q.calls.length]), ...calls]);
    return Buffer.concat([u16(q.chain), Buffer.from([1]), v(body)]);
  });
  return Buffer.concat([Buffer.from([1]), u32(nonce), Buffer.from([perChain.length]), ...qs]);
}

/** What the board wants per feed: latestRoundData() then decimals(), on the feed's proxy. */
export function feedCalls(f: Feed): { to: string; data: string }[] {
  return [{ to: f.address, data: LATEST_ROUND_DATA }, { to: f.address, data: DECIMALS }];
}
