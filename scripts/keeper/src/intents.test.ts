import { test } from "node:test";
import assert from "node:assert/strict";
import { afterFee, gross, rfc3339, versionedNonce } from "./intents.ts";

test("gross-up: what's left after the fee is exactly the net", () => {
  for (const net of [95n * 10n ** 18n, 1n, 999_999n, 123_456_789n]) {
    const g = gross(net, 1n);
    assert.equal(afterFee(g, 1n), net);
    assert.ok(afterFee(g - 1n, 1n) < net, "and it's the smallest such amount");
  }
});

test("dates and nonces match the contract's format", () => {
  assert.equal(rfc3339(0n), "1970-01-01T00:00:00Z");
  assert.equal(rfc3339(1_782_395_622_123_456_789n), "2026-06-25T13:53:42Z");
  const n = versionedNonce(Buffer.from([1, 2, 3, 4]), 0x0102030405060708n, Buffer.alloc(15, 9));
  assert.equal(n.length, 32);
  assert.equal(n.subarray(0, 5).toString("hex"), "5628f6c600");
});
