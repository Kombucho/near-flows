// Keeper + market simulator.
//
//   npm run keeper -- --mode simulate --scenario pump_dump --minutes 20          (dry run: prints only)
//   npm run keeper -- --mode simulate --scenario pump_dump --minutes 20 --send   (sends transactions)
//   npm run keeper -- --mode live --ref-contract v2.ref-finance.near --ref-pool 123 --send
//
// Every tick it:
//   1. works out the outside ("fair") price: SIMULATED from a scenario on
//      testnet, or read from a Rhea pool in live mode;
//   2. trades the pool toward it while the gap beats twice the current fee
//      (that trading is what fills holders' take-profit rungs);
//   3. pays out filled liquidity to its owners (`settle`), so proceeds just
//      land in their wallets with no claim step;
//   4. calls the public crank when an hour has passed (dividends, burn, depth).
//
// Environment: MARKET_ID, KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC (default FastNEAR testnet), KEEPER_PRIVATE_KEY (or a near-cli key file).

import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { decide, rng, scalePrice, scenarioFactor, type Scenario } from "./logic.ts";

const { values: opt } = parseArgs({
  options: {
    mode: { type: "string", default: "simulate" },
    scenario: { type: "string", default: "pump_dump" },
    minutes: { type: "string", default: "20" },
    tick: { type: "string", default: "15" }, // seconds between ticks
    chunk: { type: "string", default: "1" }, // quote tokens per trade (human units)
    slip: { type: "string", default: "500" }, // bps
    "max-trades": { type: "string", default: "3" }, // per tick
    "crank-every": { type: "string", default: "3600" }, // seconds
    seed: { type: "string", default: "7" },
    "ref-contract": { type: "string" },
    "ref-pool": { type: "string" },
    send: { type: "boolean", default: false },
    once: { type: "boolean", default: false },
  },
});

const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com";
const market = process.env.MARKET_ID ?? fail("set MARKET_ID");
const keeperId = process.env.KEEPER_ACCOUNT_ID;
const simulate = opt.mode === "simulate";
const tag = simulate ? "[SIMULATION]" : "[LIVE]";

function fail(msg: string): never {
  console.error(msg);
  process.exit(1);
}

const sleep = (s: number) => new Promise((r) => setTimeout(r, s * 1000));
const ratio = (a: bigint, b: bigint) => (Number((a * 10_000n) / b) / 10_000).toFixed(3) + "x";

async function main() {
  const key = opt.send ? loadKey(network, keeperId ?? fail("set KEEPER_ACCOUNT_ID to send")) : undefined;
  const chain = new Chain(rpc, market, keeperId, key);
  const cfg = await chain.config();
  if (!cfg.launched) fail("market not launched yet");
  const chunk = BigInt(Math.round(Number(opt.chunk) * 1e6)) * 10n ** BigInt((await chain.decimals(cfg.quote)) - 6);
  const p0 = BigInt(cfg.price);
  const rand = rng(Number(opt.seed));
  const duration = Number(opt.minutes) * 60;
  const start = Date.now();
  let lastCrank = 0;
  console.log(`${tag} market ${market} | start price ${cfg.price} | ${opt.send ? "SENDING" : "dry run (add --send)"}`);

  for (;;) {
    const elapsed = (Date.now() - start) / 1000;
    if (simulate && elapsed > duration) break;
    const fair = simulate
      ? scalePrice(p0, scenarioFactor(opt.scenario as Scenario, elapsed / duration, rand()))
      : await chain.refPrice(opt["ref-contract"] ?? fail("--ref-contract"), Number(opt["ref-pool"]), cfg.token, cfg.quote);
    await tradeTowards(chain, cfg.token, cfg.quote, fair, p0, chunk);
    await settleFilled(chain);
    if (Date.now() / 1000 - lastCrank >= Number(opt["crank-every"])) {
      lastCrank = Date.now() / 1000;
      if (opt.send) await attempt("crank", () => chain.crank());
      else console.log(`${tag}   would crank (dry run)`);
    }
    if (opt.once) break;
    await sleep(Number(opt.tick));
  }
  console.log(`${tag} done`);
}

/** Up to `max-trades` trades this tick, re-reading the pool after each one. */
async function tradeTowards(chain: Chain, token: string, quote: string, fair: bigint, p0: bigint, chunk: bigint) {
  for (let i = 0; i < Number(opt["max-trades"]); i++) {
    const c = await chain.config();
    if (c.paused) return console.log(`${tag} market paused: not trading`);
    const book = BigInt(c.price);
    const a = decide(book, fair, BigInt(c.current_fee_bps), chunk, BigInt(opt.slip));
    const where = `fair ${ratio(fair, p0)} book ${ratio(book, p0)} fee ${c.current_fee_bps}bps`;
    if (a.kind === "hold") return console.log(`${tag} ${where} -> hold (${a.reason})`);
    if (a.kind === "buy") {
      console.log(`${tag} ${where} -> BUY ${a.quoteIn} quote (min out ${a.minOut})`);
      if (opt.send && (await hasBalance(chain, quote, a.quoteIn))) await attempt("buy", () => chain.buy(quote, a.quoteIn, a.minOut));
    } else {
      console.log(`${tag} ${where} -> SELL ${a.tokenIn} tokens (min out ${a.minOut})`);
      if (opt.send && (await hasBalance(chain, token, a.tokenIn))) await attempt("sell", () => chain.sell(token, a.tokenIn, a.minOut));
    }
    if (!opt.send) return; // dry run: one line per tick is enough
  }
}

/** Pay owners whose liquidity was traded. At most 10 per tick to bound gas spend. */
async function settleFilled(chain: Chain) {
  const ids = await chain.settleable(10);
  if (ids.length === 0) return;
  if (!opt.send) return console.log(`${tag}   would settle positions ${ids.join(", ")} (dry run)`);
  for (const id of ids) await attempt(`settle #${id} (paid to its owner)`, () => chain.settle(id));
}

async function hasBalance(chain: Chain, token: string, need: bigint): Promise<boolean> {
  const have = await chain.balance(token, chain.keeperId);
  if (have < need) console.log(`${tag}   not enough ${token} (have ${have}, need ${need}): skipping`);
  return have >= need;
}

/** Run a transaction; log failures (e.g. crank "too soon") without stopping. */
async function attempt(what: string, fn: () => Promise<unknown>) {
  try {
    await fn();
    console.log(`${tag}   ${what}: ok`);
  } catch (e) {
    console.log(`${tag}   ${what}: failed (${(e as Error).message.slice(0, 160)})`);
  }
}

main().catch((e) => fail(`keeper stopped: ${(e as Error).message}`));
