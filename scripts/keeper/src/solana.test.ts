import { test } from "node:test";
import assert from "node:assert/strict";
import { messageOf, shortvec, withSignature } from "./solana.ts";

test("shortvec reads 1-3 byte lengths", () => {
  assert.deepEqual(shortvec(Uint8Array.from([0x05])), [5, 1]);
  assert.deepEqual(shortvec(Uint8Array.from([0x80, 0x01])), [128, 2]);
  assert.deepEqual(shortvec(Uint8Array.from([0xff, 0xff, 0x03])), [65535, 3]);
});

test("a signature goes in and the same message comes back out", () => {
  const msg = Uint8Array.from([1, 0, 0, 1, ...new Array(32).fill(7), ...new Array(32).fill(9), 0]);
  const sig = new Uint8Array(64).fill(3);
  const tx = withSignature(msg, sig);
  assert.equal(tx.length, 1 + 64 + msg.length);
  assert.deepEqual(messageOf(tx), msg);
  assert.throws(() => withSignature(msg, new Uint8Array(63)));
});
