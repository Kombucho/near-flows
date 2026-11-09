// Uniswap v3 TWAPs as a feed's BACKUP on the price board (contracts/priceboard): the
// pool, how to read its time-weighted mean tick with observe([window, 0]), and the
// tick as a price. Pure functions, tested in twap.test.ts.
//
// WHY a pool TWAP behind NVDA: Chainlink's RHNVDA/USD on Robinhood Chain is the only
// NVDA token price on a chain we can read, and a market floor needs a second,
// independent publisher. The NVDA/USDG 0.05% pool there (~9,300 NVDA + ~147k USDG,
// 7,200 observations) is one: a 30-minute TWAP can't be moved in one block, and moving
// it for half an hour costs whoever tries the pool's depth for that long. It only
// CONFIRMS the price (the brain refuses when the two disagree by more than its
// tolerance, 2%); the floor itself is Chainlink's, so a bent TWAP can stop a trade,
// never cheapen one. USDG is taken as $1 (Paxos' USD stablecoin; no feed prices it).
//
// The board's price id is the Chainlink scheme (chainlink.ts priceId): Wormhole chain
// id, 10 zero bytes, the POOL's address. The read is one eth_call (observe) at a
// pinned block, so an Evm board could check a Wormhole Query of it the same way it
// checks latestRoundData() (not built yet: until then the keeper attests it).

/** keccak256("observe(uint32[])")[..4] */
export const OBSERVE = "883bdbfd";

export type Twap = {
  description: string;
  /** Wormhole chain id (the price id's), and the EVM chain id (checked against the RPC). */
  chain: number;
  evmChainId: number;
  pool: string;
  /** The pool's tokens (checked on every read) and their decimals. */
  token0: string;
  token1: string;
  decimals0: number;
  decimals1: number;
  /** Which token is priced (the other is the dollar). */
  base: 0 | 1;
  windowS: number;
  rpc: string;
  rpcEnv: string;
};

// Verified 2026-11-09 against rpc.mainnet.chain.robinhood.com: factory 0x1f7d...2EfA's
// getPool(USDG, NVDA, 500) is this pool; token0 USDG (6), token1 NVDA (18), fee 500,
// observation cardinality 7,200; observe([1800, 0]) at block 109,995,929 gave a mean tick
// of 224,463 = $178.72 against Chainlink RHNVDA's $179.02 (-0.17%).
export const TWAPS: Record<string, Twap> = {
  "nvda-twap": {
    description: "NVDA/USDG 0.05% Uniswap v3, 30-min TWAP", chain: 72, evmChainId: 4663,
    pool: "0xd4eb21209c4d6093f80b5b84f5c45cc093ea14a3",
    token0: "0x5fc5360d0400a0fd4f2af552add042d716f1d168", token1: "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec",
    decimals0: 6, decimals1: 18, base: 1, windowS: 1800,
    rpc: "https://rpc.mainnet.chain.robinhood.com", rpcEnv: "ROBINHOOD_RPC",
  },
};

/** observe([secondsAgo, 0]) calldata. */
export function observeCall(windowS: number): string {
  const w = (n: number | bigint) => BigInt(n).toString(16).padStart(64, "0");
  return OBSERVE + w(32) + w(2) + w(windowS) + w(0);
}

/** The two tickCumulatives (int56) from observe's answer: (int56[], uint160[]). */
export function decodeObserve(hex: string): [bigint, bigint] {
  const h = hex.replace(/^0x/, "");
  const word = (i: number) => BigInt("0x" + h.slice(i * 64, (i + 1) * 64));
  const signed = (v: bigint) => (v >= 1n << 255n ? v - (1n << 256n) : v);
  const at = Number(word(0)) / 32; // offset of the first array, in words
  if (word(at) !== 2n) throw new Error("observe: expected two tickCumulatives");
  return [signed(word(at + 1)), signed(word(at + 2))];
}

/** Uniswap's OracleLibrary.consult: the mean tick, rounded toward negative infinity. */
export function meanTick(older: bigint, newer: bigint, windowS: number): number {
  const d = newer - older;
  const w = BigInt(windowS);
  let t = d / w;
  if (d < 0n && d % w !== 0n) t -= 1n;
  return Number(t);
}

/** Uniswap's TickMath.getSqrtRatioAtTick, exactly (sqrt(1.0001^tick) x 2^96). */
export function sqrtRatioAtTick(tick: number): bigint {
  const abs = BigInt(Math.abs(tick));
  if (abs > 887272n) throw new Error("tick out of range");
  const M = [
    0xfffcb933bd6fad37aa2d162d1a594001n, 0xfff97272373d413259a46990580e213an, 0xfff2e50f5f656932ef12357cf3c7fdccn, 0xffe5caca7e10e4e61c3624eaa0941cd0n,
    0xffcb9843d60f6159c9db58835c926644n, 0xff973b41fa98c081472e6896dfb254c0n, 0xff2ea16466c96a3843ec78b326b52861n, 0xfe5dee046a99a2a811c461f1969c3053n,
    0xfcbe86c7900a88aedcffc83b479aa3a4n, 0xf987a7253ac413176f2b074cf7815e54n, 0xf3392b0822b70005940c7a398e4b70f3n, 0xe7159475a2c29b7443b29c7fa6e889d9n,
    0xd097f3bdfd2022b8845ad8f792aa5825n, 0xa9f746462d870fdf8a65dc1f90e061e5n, 0x70d869a156d2a1b890bb3df62baf32f7n, 0x31be135f97d08fd981231505542fcfa6n,
    0x9aa508b5b7a84e1c677de54f3e99bc9n, 0x5d6af8dedb81196699c329225ee604n, 0x2216e584f5fa1ea926041bedfe98n, 0x48a170391f7dc42444e8fa2n,
  ];
  let ratio = abs & 1n ? M[0] : 1n << 128n;
  for (let i = 1; i < 20; i++) if (abs & (1n << BigInt(i))) ratio = (ratio * M[i]) >> 128n;
  if (tick > 0) ratio = ((1n << 256n) - 1n) / ratio;
  return (ratio >> 32n) + (ratio % (1n << 32n) === 0n ? 0n : 1n);
}

/** The base token's price in the other (a dollar), x 10^`digits`, at `tick`. */
export function tickPrice(tick: number, t: Pick<Twap, "decimals0" | "decimals1" | "base">, digits = 8): bigint {
  const s = sqrtRatioAtTick(tick);
  const q192 = 1n << 192n;
  // token1 raw per token0 raw = s^2 / 2^192.
  if (t.base === 1) {
    // token0 per whole token1 = 2^192 / s^2 x 10^(dec1 - dec0).
    const e = t.decimals1 - t.decimals0 + digits;
    return e >= 0 ? (q192 * 10n ** BigInt(e)) / (s * s) : q192 / (s * s * 10n ** BigInt(-e));
  }
  const e = t.decimals0 - t.decimals1 + digits;
  return e >= 0 ? (s * s * 10n ** BigInt(e)) / q192 : (s * s) / (q192 * 10n ** BigInt(-e));
}
