// Run: npm test   (uses Node's built-in test runner)
import { test } from "node:test";
import assert from "node:assert/strict";
import { decide, minOutBuy, minOutSell, rng, scalePrice, scenarioFactor, SCALE } from "./logic.ts";

const P1 = SCALE; // price 1.0

test("holds when the gap is smaller than twice the fee", () => {
  // fee 0.3% -> band 0.6%. Fair 0.5% above book: not worth it.
  const a = decide(P1, (P1 * 1005n) / 1000n, 30n, 10n ** 18n, 500n);
  assert.equal(a.kind, "hold");
});

test("buys when the pool is cheaper than outside", () => {
  const a = decide(P1, (P1 * 110n) / 100n, 30n, 10n ** 18n, 500n);
  assert.equal(a.kind, "buy");
  if (a.kind === "buy") assert.equal(a.quoteIn, 10n ** 18n);
});

test("sells when the pool is dearer than outside", () => {
  const a = decide((P1 * 120n) / 100n, P1, 30n, 12n * 10n ** 17n, 500n);
  assert.equal(a.kind, "sell");
  if (a.kind === "sell") assert.equal(a.tokenIn, 10n ** 18n, "1.2 quote of tokens at price 1.2 = 1 token");
});

test("min out for a buy: 100 quote at price 2, 0.3% fee, 5% slippage", () => {
  // 100 * 0.997 = 99.7 quote -> 49.85 tokens -> *0.95 = 47.3575
  assert.equal(minOutBuy(100n * SCALE, 2n * SCALE, 30n, 500n), 473575n * 10n ** 14n);
});

test("min out for a sell: 10 tokens at price 2, 0.3% fee, 5% slippage", () => {
  // 20 quote * 0.997 * 0.95 = 18.943
  assert.equal(minOutSell(10n * SCALE, 2n * SCALE, 30n, 500n), 18943n * 10n ** 15n);
});

test("pump_dump scenario goes 1x -> 3x -> 0.5x", () => {
  const mid = 0.5; // no jitter
  assert.ok(Math.abs(scenarioFactor("pump_dump", 0, mid) - 1) < 1e-9);
  assert.ok(Math.abs(scenarioFactor("pump_dump", 0.4, mid) - 3) < 1e-9);
  assert.ok(Math.abs(scenarioFactor("pump_dump", 1, mid) - 0.5) < 1e-9);
});

test("the random generator is replayable from its seed", () => {
  const a = rng(42), b = rng(42);
  for (let i = 0; i < 5; i++) assert.equal(a(), b());
});

test("scalePrice multiplies a bigint price by a float factor", () => {
  assert.equal(scalePrice(2n * SCALE, 1.5), 3n * SCALE);
  assert.equal(scalePrice(SCALE, 0.5), SCALE / 2n);
});
