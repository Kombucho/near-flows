// The same real vectors the contract's tests use (contracts/priceboard/tests-data):
// Chainlink reads recorded by eth_call on 2026-11-06, and a real Wormhole Query
// response whose request we rebuild byte for byte.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { decodeDecimals, decodeRound, decodeString, ethCallRequest, feedCalls, FEEDS, phaseRound, priceId, stale } from "./chainlink.ts";
import { attestation } from "./prices.ts";

const data = (f: string) => JSON.parse(readFileSync(new URL(`../../../contracts/priceboard/tests-data/${f}`, import.meta.url), "utf8"));
const reads = data("chainlink_reads.json").feeds;

test("a feed's price id is its Wormhole chain, ten zero bytes and its proxy, as the board computes it", () => {
  assert.equal(priceId(72, "0x379EC4f7C378F34a1B47E4F3cbeBCbAC3E8E9F15"), "004800000000000000000000379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15");
  assert.equal(priceId(FEEDS.near.chain, FEEDS.near.address), "001700000000000000000000bf5c3fb2633e924598a46b9d07a174a9dbcf57c0");
  // The backups the deploy script names (scripts/deploy-relay-testnet.sh): Chainlink on Arbitrum behind Pyth's ETH, SOL, USDC.
  assert.equal(priceId(FEEDS["eth-arb"].chain, FEEDS["eth-arb"].address), "001700000000000000000000639fe6ab55c921f74e7fac1ee960c0b6293ba612");
  assert.equal(priceId(FEEDS["sol-arb"].chain, FEEDS["sol-arb"].address), "00170000000000000000000024cea4b8ce57cda5058b924b9b9987992450590c");
  assert.equal(priceId(FEEDS["usdc-arb"].chain, FEEDS["usdc-arb"].address), "00170000000000000000000050834f3163758fcc1df9973b6e91f0f0f0434ad3");
  assert.equal(priceId(FEEDS["near-op"].chain, FEEDS["near-op"].address), "001800000000000000000000ca6fa4b8cb365c02cd3ba70544efffe78f63ac82");
  assert.throws(() => priceId(0, FEEDS.near.address));
  assert.throws(() => priceId(23, "0x1234"));
});

test("the recorded NVDA and NEAR reads decode to their prices, decimals and descriptions", () => {
  const n = reads.nvda_robinhood;
  const r = decodeRound(n.latestRoundData);
  assert.deepEqual([r.answer, r.updatedAt, decodeDecimals(n.decimals), decodeString(n.description)], [18955682289n, 1793979985n, 8, "RHNVDA / USD"]);
  assert.deepEqual(phaseRound(r.roundId), { phase: 1n, round: 1584n });
  assert.equal(n.address, FEEDS.nvda.address);
  const a = reads.near_arbitrum;
  const s = decodeRound(a.latestRoundData);
  assert.deepEqual([s.answer, decodeDecimals(a.decimals), decodeString(a.description)], [375297364n, 8, "NEAR / USD"]);
  assert.equal(a.address, FEEDS.near.address);
});

test("a negative or zero answer is not a price, and an answer older than the heartbeat is not signed", () => {
  const w = reads.nvda_robinhood.latestRoundData.slice(2);
  const withAnswer = (word: string) => w.slice(0, 64) + word + w.slice(128);
  assert.throws(() => decodeRound(withAnswer("f".repeat(64))), /-1 is not a price/);
  assert.throws(() => decodeRound(withAnswer("0".repeat(64))), /0 is not a price/);
  assert.throws(() => decodeRound(w.slice(0, 256)), /5 words/);
  const r = decodeRound(w);
  assert.equal(stale(r, 1793980513, 86_400), null); // 528 s old: fine
  assert.match(stale(r, 1793980513, 300)!, /528 s old at the block, heartbeat 300 s/);
  assert.match(stale(r, 1793979900, 86_400)!, /after its block/);
});

test("an eth_call query request matches a real one byte for byte", () => {
  // The request inside wethNameResponse: nonce 1, Ethereum (2), block 0x124c0d6, WETH's name().
  const real = data("mainnet_queries.json").wethNameResponse.bytes as string;
  const len = parseInt(real.slice(2 + 4 + 130, 2 + 4 + 130 + 8), 16);
  const request = real.slice(2 + 4 + 130 + 8, 2 + 4 + 130 + 8 + len * 2);
  const ours = ethCallRequest([{ chain: 2, block: "0x124c0d6", calls: [{ to: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2", data: "06fdde03" }] }], 1);
  assert.equal(Buffer.from(ours).toString("hex"), request);
});

test("a feed is queried as latestRoundData() then decimals() on its proxy, two chains in one request", () => {
  const q = ethCallRequest([{ chain: 72, block: "0x66b6fe5", calls: feedCalls(FEEDS.nvda) }, { chain: 23, block: "0x1f1aa110", calls: feedCalls(FEEDS.near) }], 7);
  const h = Buffer.from(q).toString("hex");
  assert.ok(h.startsWith("01" + "00000007" + "02" + "0048" + "01"));
  assert.ok(h.includes("379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15" + "00000004feaf968c" + "379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15" + "00000004313ce567"));
  assert.ok(h.includes("0017" + "01"));
});

test("an attested Chainlink price signs the feed's id, the answer, -decimals and the block time", () => {
  const id = priceId(72, FEEDS.nvda.address);
  assert.equal(Buffer.from(attestation(id, 18955682289n, -8, 1793980513)).toString("hex"),
    "4e454152464c4f57535f50524943455f5631" + id + "0000000469d8c1f1" + "fffffff8" + "000000006aedf861");
});

test("every backup is a different publisher's feed from its primary, and no two feeds share an id", () => {
  const ids = Object.values(FEEDS).map((f) => priceId(f.chain, f.address));
  assert.equal(new Set(ids).size, ids.length);
  // NEAR's backup is the same pair on another chain: its own proxy, its own updates.
  assert.notEqual(FEEDS.near.chain, FEEDS["near-op"].chain);
  for (const n of ["eth-arb", "sol-arb", "usdc-arb"]) assert.match(FEEDS[n].description, /^(ETH|SOL|USDC) \/ USD$/);
  // NVDA has no second Chainlink feed here: its backup is a pool TWAP (twap.ts).
  assert.deepEqual(Object.values(FEEDS).filter((f) => /NVDA/.test(f.description)).length, 1);
});
