// Solana message bytes: pull the message out of a wire transaction, and put a
// signature back in. Pure, so it can be tested.

/** Read Solana's compact-u16 ("shortvec"). Returns [value, bytes used]. */
export function shortvec(b: Uint8Array, at = 0): [number, number] {
  let v = 0;
  for (let k = 0; k < 3; k++) {
    const x = b[at + k];
    if (x === undefined) throw new Error("short shortvec");
    v |= (x & 0x7f) << (7 * k);
    if ((x & 0x80) === 0) return [v, k + 1];
  }
  throw new Error("bad shortvec");
}

/** The message inside a transaction: [n][n x 64-byte signatures][message]. */
export function messageOf(tx: Uint8Array): Uint8Array {
  const [n, used] = shortvec(tx);
  return tx.slice(used + 64 * n);
}

/** A transaction with one signature (the vault's) around `message`. */
export function withSignature(message: Uint8Array, signature: Uint8Array): Uint8Array {
  if (signature.length !== 64) throw new Error("an Ed25519 signature is 64 bytes");
  const out = new Uint8Array(1 + 64 + message.length);
  out[0] = 1;
  out.set(signature, 1);
  out.set(message, 65);
  return out;
}
