// NEAR Intents helpers (pure): the same formats as contracts/intentsig, for a solver.

import { createHash } from "node:crypto";

const NEP413_TAG = 2 ** 31 + 413;
const MAGIC = Buffer.from("5628f6c6", "hex");

const u32 = (n: number) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const str = (s: string) => Buffer.concat([u32(Buffer.byteLength(s)), Buffer.from(s)]);

export function nep413Hash(message: string, nonce: Buffer, recipient: string): Buffer {
  return createHash("sha256").update(Buffer.concat([u32(NEP413_TAG), str(message), nonce, str(recipient), Buffer.from([0])])).digest();
}

export function versionedNonce(salt: Buffer, deadlineNs: bigint, random: Buffer): Buffer {
  const d = Buffer.alloc(8);
  d.writeBigInt64LE(deadlineNs);
  return Buffer.concat([MAGIC, Buffer.from([0]), salt, d, random.subarray(0, 15)]);
}

export function rfc3339(ns: bigint): string {
  return new Date(Number(ns / 1_000_000n)).toISOString().replace(/\.\d{3}Z$/, "Z");
}

export function swapMessage(signer: string, deadlineNs: bigint, give: [string, bigint], get: [string, bigint]): string {
  return JSON.stringify({ signer_id: signer, deadline: rfc3339(deadlineNs),
    intents: [{ intent: "token_diff", diff: { [`nep141:${give[0]}`]: `-${give[1]}`, [`nep141:${get[0]}`]: `${get[1]}` } }] });
}

/** The fee is charged on what a signer gives, rounded up: the gross G such that G - fee(G) = net. */
export function gross(net: bigint, feePips: bigint): bigint {
  const fee = (g: bigint) => (g * feePips + 999_999n) / 1_000_000n;
  let g = (net * 1_000_000n) / (1_000_000n - feePips);
  while (g - fee(g) < net) g++;
  while (g > 0n && g - 1n - fee(g - 1n) >= net) g--;
  return g;
}
export const afterFee = (amount: bigint, feePips: bigint) => amount - (amount * feePips + 999_999n) / 1_000_000n;
