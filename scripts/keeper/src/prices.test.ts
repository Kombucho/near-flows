// Expected bytes built independently (Python struct) and the same layouts the
// contract's own tests parse (contracts/priceboard, contracts/sandbox/tests/priceboard.rs).
import { test } from "node:test";
import assert from "node:assert/strict";
import { KeyPair } from "near-api-js";
import { createPublicKey, verify } from "node:crypto";
import { attestation, curveQuery, feedId, toUnits, unsignable } from "./prices.ts";

test("a Hermes price is relayed only if positive and no less certain than the limit", () => {
  assert.equal(unsignable({ price: "300000000000", conf: "150000000" }, 100), null); // 0.05%
  assert.match(unsignable({ price: "300000000000", conf: "3000000001" }, 100)!, /wider than 100 bps/);
  assert.match(unsignable({ price: "0", conf: "0" }, 100)!, /not positive/);
});

const MEME = "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump";
const MEME_HEX = "e202a830669968911d12fb8957d8651fcfa7a56a28cb6b84b7fc21239d8859df";

test("a coin's feed id is its mint in hex", () => {
  assert.equal(feedId(MEME), MEME_HEX);
});

test("attestation bytes: domain, feed, price, expo, time, big-endian", () => {
  const m = attestation(MEME_HEX, 51204n, -12, 1793960711);
  assert.equal(Buffer.from(m).toString("hex"),
    "4e454152464c4f57535f50524943455f5631" + MEME_HEX + "000000000000c804" + "fffffff4" + "000000006aedab07");
});

test("a NEAR ed25519 key's signature over them checks out as plain Ed25519", () => {
  const kp = KeyPair.fromRandom("ed25519");
  const m = attestation(MEME_HEX, 51204n, -12, 1793960711);
  const sig = kp.sign(m).signature;
  const raw = kp.getPublicKey().data;
  const der = Buffer.concat([Buffer.from("302a300506032b6570032100", "hex"), Buffer.from(raw)]);
  assert.ok(verify(null, m, createPublicKey({ key: der, format: "der", type: "spki" }), sig));
});

test("Jupiter prices become board integers without float drift", () => {
  assert.equal(toUnits(5.467173667877699e-8), 54672n); // 54671.7 -> 54672
  assert.equal(toUnits(89.915985604039), 89915985604039n);
  assert.equal(toUnits(0.1 + 0.2, -2), 30n); // 0.30000000000000004 -> 30 cents
  assert.equal(toUnits(1e-13), 0n); // below the board's resolution: rounds to 0 (the board refuses a zero price)
  assert.throws(() => toUnits(0));
  assert.throws(() => toUnits(-1));
});

test("a Wormhole sol_pda query for a pump.fun curve", () => {
  assert.equal(Buffer.from(curveQuery([MEME], 7)).toString("hex"),
    "0100000007010001050000007c0000000966696e616c697a6564" + "00".repeat(24) + "01" +
    "0156e0f693665acf44db1568bf175baa5189cb97f5d2ff3b655d2bb6fd6d18b0" + "02" +
    "0000000d626f6e64696e672d6375727665" + "00000020" + MEME_HEX);
});
