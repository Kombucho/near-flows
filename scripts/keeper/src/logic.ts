// Pure decision logic for the keeper. No network, no keys: easy to test.
//
// Units: everything is a bigint in the tokens' smallest units.
// A "price" is quote-per-token scaled by 1e18 (the same format the contract's
// get_config returns), so price = 1e18 means 1 quote unit per token unit.

export const SCALE = 10n ** 18n;
export const BPS = 10_000n;

export type Action =
  | { kind: "buy"; quoteIn: bigint; minOut: bigint }
  | { kind: "sell"; tokenIn: bigint; minOut: bigint }
  | { kind: "hold"; reason: string };

/**
 * Should we trade the pool toward the outside ("fair") price?
 * Only when the gap beats twice the current fee: below that, a trade loses
 * money to fees, so a real arbitrage bot would not do it either.
 */
export function decide(book: bigint, fair: bigint, feeBps: bigint, chunkQuote: bigint, slipBps: bigint): Action {
  if (book <= 0n || fair <= 0n) return { kind: "hold", reason: "no price" };
  const band = 2n * feeBps;
  if (book * BPS < fair * (BPS - band)) {
    // Pool is cheaper than outside: buy here.
    return { kind: "buy", quoteIn: chunkQuote, minOut: minOutBuy(chunkQuote, book, feeBps, slipBps) };
  }
  if (book * BPS > fair * (BPS + band)) {
    // Pool is dearer than outside: sell here, a chunk worth `chunkQuote`.
    const tokenIn = (chunkQuote * SCALE) / book;
    return { kind: "sell", tokenIn, minOut: minOutSell(tokenIn, book, feeBps, slipBps) };
  }
  return { kind: "hold", reason: "gap smaller than 2x fee" };
}

/** Least tokens we accept for `quoteIn` at `price`, after fee and slippage. */
export function minOutBuy(quoteIn: bigint, price: bigint, feeBps: bigint, slipBps: bigint): bigint {
  const afterFee = (quoteIn * (BPS - feeBps)) / BPS;
  return (afterFee * SCALE * (BPS - slipBps)) / (price * BPS);
}

/** Least quote we accept for `tokenIn` at `price`, after fee and slippage. */
export function minOutSell(tokenIn: bigint, price: bigint, feeBps: bigint, slipBps: bigint): bigint {
  const gross = (tokenIn * price) / SCALE;
  return (gross * (BPS - feeBps) * (BPS - slipBps)) / (BPS * BPS);
}

/** Scale a bigint price by a float factor (e.g. 1.5x), keeping 9 digits. */
export function scalePrice(price: bigint, factor: number): bigint {
  return (price * BigInt(Math.round(factor * 1e9))) / 1_000_000_000n;
}

/** Small seeded random generator, so a simulated scenario can be replayed. */
export function rng(seed: number): () => number {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13; s >>>= 0;
    s ^= s >>> 17;
    s ^= s << 5; s >>>= 0;
    return s / 4_294_967_296;
  };
}

export type Scenario = "calm" | "pump_dump" | "chop";

/**
 * The SIMULATED outside price, as a multiple of the starting price, at
 * `progress` (0 = start, 1 = end of the scenario). `noise` adds jitter.
 *   calm:      drifts within a few percent
 *   pump_dump: rises to 3x by 40%, then falls to 0.5x by the end
 *   chop:      swings +-30% around the start, three times
 */
export function scenarioFactor(s: Scenario, progress: number, noise: number): number {
  const p = Math.min(Math.max(progress, 0), 1);
  const jitter = 1 + (noise - 0.5) * 0.04; // +-2%
  if (s === "calm") return jitter;
  if (s === "chop") return (1 + 0.3 * Math.sin(p * 6 * Math.PI)) * jitter;
  // pump_dump, as a smooth log-path: 1 -> 3 (at 0.4) -> 0.5 (at 1)
  const logF = p <= 0.4 ? Math.log(3) * (p / 0.4) : Math.log(3) + (Math.log(0.5) - Math.log(3)) * ((p - 0.4) / 0.6);
  return Math.exp(logF) * jitter;
}
