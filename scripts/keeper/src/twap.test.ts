// A real observe([1800, 0]) answer from the NVDA/USDG pool on Robinhood Chain (block
// 110,004,030, 2026-11-09), Uniswap's own TickMath constants, and the price id scheme.
import { test } from "node:test";
import assert from "node:assert/strict";
import { decodeObserve, meanTick, observeCall, sqrtRatioAtTick, tickPrice, TWAPS } from "./twap.ts";
import { priceId } from "./chainlink.ts";

const RAW =
  "0x0000000000000000000000000000000000000000000000000000000000000040" +
  "00000000000000000000000000000000000000000000000000000000000000a0" +
  "0000000000000000000000000000000000000000000000000000000000000002" +
  "000000000000000000000000000000000000000000000000000001f0b9e50213" +
  "000000000000000000000000000000000000000000000000000001f0d1fa545e" +
  "0000000000000000000000000000000000000000000000000000000000000002" +
  "00000000000000000000000000000000000000003b25ffc42eb0f299519d38ac" +
  "00000000000000000000000000000000000000003b27e7bb532e4e767e1ce27e";

test("observe's calldata is cast's, and its answer decodes to the two tick cumulatives", () => {
  // cast calldata 'observe(uint32[])' '[1800,0]'
  assert.equal(observeCall(1800), "883bdbfd" + "20".padStart(64, "0") + "2".padStart(64, "0") + "708".padStart(64, "0") + "0".padStart(64, "0"));
  assert.deepEqual(decodeObserve(RAW), [2133422572051n, 2133826622558n]);
});

test("the mean tick rounds toward negative infinity, as Uniswap's OracleLibrary does", () => {
  assert.equal(meanTick(2133422572051n, 2133826622558n, 1800), 224472);
  assert.equal(meanTick(0n, -3601n, 1800), -3);
  assert.equal(meanTick(0n, -3600n, 1800), -2);
  assert.equal(meanTick(0n, 3601n, 1800), 2);
});

test("sqrtRatioAtTick is Uniswap's TickMath to the last bit", () => {
  assert.equal(sqrtRatioAtTick(0), 1n << 96n);
  assert.equal(sqrtRatioAtTick(-887272), 4295128739n); // MIN_SQRT_RATIO
  assert.equal(sqrtRatioAtTick(887272), 1461446703485210103287273052203988822378723970342n); // MAX_SQRT_RATIO
  assert.throws(() => sqrtRatioAtTick(887273));
  // The pool's slot0 at the same block: sqrtPriceX96 between its tick's ratio and the next one's.
  const spot = 5931975294164757999150870088345821n;
  assert.ok(sqrtRatioAtTick(224481) <= spot && spot < sqrtRatioAtTick(224482));
});

test("the 30-min TWAP prices NVDA at $178.56 in USDG, within 0.3% of Chainlink's $178.07 at that block", () => {
  const t = TWAPS["nvda-twap"];
  const p = tickPrice(224472, t); // x 1e8, rounded down (1e12 / 1.0001^224472 = 178.5633430173 in floats)
  assert.equal(p, 17856334301n);
  const chainlink = 17807292794n;
  assert.ok(Number(p - chainlink) / Number(chainlink) < 0.003);
  // The other way round (USDG priced in NVDA) is the inverse, within rounding.
  const inv = tickPrice(224472, { ...t, base: 0 }, 18);
  assert.ok(Math.abs(Number(inv) / 1e18 - 1 / 178.56334302) < 1e-12);
});

test("the TWAP's board price id is the Chainlink scheme with the pool's address", () => {
  const t = TWAPS["nvda-twap"];
  assert.equal(priceId(t.chain, t.pool), "004800000000000000000000d4eb21209c4d6093f80b5b84f5c45cc093ea14a3");
});
