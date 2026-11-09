// Memecoin prices for the price board (contracts/priceboard): the bytes an
// attester signs, Jupiter prices as board integers, and Wormhole Query
// requests for pump.fun bonding curves. Pure functions, tested in prices.test.ts.

import { PublicKey } from "@solana/web3.js";

export const DOMAIN = new TextEncoder().encode("NEARFLOWS_PRICE_V1");
/** Board prices: whole-coin USD x 10^EXPO (memecoins trade far below a cent). */
export const EXPO = -12;
export const PUMP_PROGRAM = new PublicKey("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");

/** A coin's feed id on the board: its Solana mint, 32 bytes hex. */
export function feedId(mint: string): string {
  return Buffer.from(new PublicKey(mint).toBytes()).toString("hex");
}

/** What an attester signs: domain, feed id, price (i64), expo (i32), publish time (i64), big-endian.
 *  Names no board or vault, so one signature serves every board. */
export function attestation(feed: string, price: bigint, expo: number, publishTime: number): Uint8Array {
  const b = Buffer.alloc(DOMAIN.length + 32 + 8 + 4 + 8);
  let at = Buffer.from(DOMAIN).copy(b, 0);
  at += Buffer.from(feed, "hex").copy(b, at);
  b.writeBigInt64BE(price, at);
  b.writeInt32BE(expo, at + 8);
  b.writeBigInt64BE(BigInt(publishTime), at + 12);
  return b;
}

/** A decimal USD price (Jupiter's usdPrice, a JS number) as an integer x 10^-expo,
 *  rounded to nearest, via its decimal digits (no float multiplication). */
export function toUnits(usd: number, expo = EXPO): bigint {
  if (!(usd > 0) || !Number.isFinite(usd)) throw new Error(`not a price: ${usd}`);
  const [mant, e] = usd.toExponential(14).split("e"); // d.dddddddddddddd e±x
  const digits = BigInt(mant.replace(".", "")); // 15 significant digits
  const shift = Number(e) - 14 - expo; // value = digits x 10^(e - 14); units = value x 10^-expo
  if (shift >= 0) return digits * 10n ** BigInt(shift);
  const d = 10n ** BigInt(-shift);
  return (digits + d / 2n) / d;
}

const u32 = (n: number) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
const u64 = (n: bigint) => { const b = Buffer.alloc(8); b.writeBigUInt64BE(n); return b; };
const v = (b: Uint8Array) => Buffer.concat([u32(b.length), b]);

/** A Wormhole Query request: read each coin's bonding curve on Solana (chain 1) at
 *  PDA ["bonding-curve", mint] of the pump program (sol_pda, type 5), finalized, whole account. */
export function curveQuery(mints: string[], nonce = Math.floor(Math.random() * 2 ** 32)): Uint8Array {
  if (!mints.length || mints.length > 255) throw new Error("1 to 255 coins per query");
  const pdas = mints.map((m) => Buffer.concat([PUMP_PROGRAM.toBuffer(), Buffer.from([2]), v(Buffer.from("bonding-curve")), v(new PublicKey(m).toBuffer())]));
  const query = Buffer.concat([v(Buffer.from("finalized")), u64(0n), u64(0n), u64(0n), Buffer.from([mints.length]), ...pdas]);
  return Buffer.concat([Buffer.from([1]), u32(nonce), Buffer.from([1]), Buffer.from([0, 1, 5]), v(query)]);
}

/** Why a Pyth (Hermes) price shouldn't be relayed to an attested board (null: fine).
 *  The board drops Pyth's confidence (it keeps its attesters' spread), so a price
 *  Pyth itself is unsure of is refused here instead. */
export function unsignable(p: { price: string; conf: string }, maxConfBps: number): string | null {
  const price = BigInt(p.price);
  if (price <= 0n) return `price ${p.price} is not positive`;
  if (BigInt(p.conf) * 10_000n > price * BigInt(maxConfBps)) return `confidence ${p.conf} is wider than ${maxConfBps} bps of ${p.price}`;
  return null;
}

type OracleFeed = { contract: string; price_id: string; backup?: { contract?: string; price_id?: string } };

/** Which of a vault's Pyth feeds `npm run pyth` posts to `target`: every feed (or backup)
 *  the vault reads there. On a pyth_core board, those the board lists (a signed update
 *  can carry any Pyth id); on an attested one, the Pyth ids we know (`known`): its Chainlink
 *  feeds are chainlink-price's. No board (Pyth's own contract): the vault's feeds there. */
export function pythBoardIds(feeds: OracleFeed[], target: string, source: { kind: string; feeds?: string[] } | null, known: string[]): string[] {
  const read = feeds.flatMap((f) => [f.contract === target ? f.price_id : "", source && f.backup?.contract === target ? f.backup.price_id ?? "" : ""]).filter(Boolean);
  const keep = !source ? () => true : source.kind === "pyth_core" ? (id: string) => (source.feeds ?? []).includes(id) : (id: string) => known.includes(id);
  return [...new Set(read.map((id) => id.toLowerCase()))].filter(keep);
}
