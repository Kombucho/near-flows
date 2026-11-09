// Post Uniswap v3 TWAPs to a price board (contracts/priceboard) as a feed's BACKUP: NVDA's
// 30-minute TWAP on the NVDA/USDG pool on Robinhood Chain, behind Chainlink's RHNVDA/USD.
// Run on demand, just before a step (flows --board does, with chainlink-price): the board
// keeps no average of its own; the average here is the POOL's (observe, on chain).
//
//   npm run twap-price -- --board <board> nvda-twap      read, sign, post
//   npm run twap-price -- --board <board> --vault <vault> every TWAP that vault reads from the board
//   ... --dry                                             read and print, send nothing
//
// The price id is the Chainlink scheme (Wormhole chain 72, 10 zero bytes, the pool's address),
// so a vault's backup names it like any Chainlink feed. ATTESTED boards only, for now: we read
// observe([1800, 0]) with a plain eth_call at a pinned block and sign what we read, and the
// board trusts us (k of its attesters) for it. Anyone can audit a price: the log gives the
// block, the two tick cumulatives and the mean tick; replay the eth_call at that block. An Evm
// board would need its own check of a Wormhole Query of observe (not built yet), so it refuses.
//
// Environment: KEEPER_ACCOUNT_ID, ATTESTER_PRIVATE_KEY (default: the keeper's key), NEAR_NETWORK
// (default testnet), NEAR_RPC, ROBINHOOD_RPC (overrides the public RPC).

import { parseArgs } from "node:util";
import { KeyPair, type KeyPairString } from "near-api-js";
import { Chain, loadKey } from "./chain.ts";
import { priceId } from "./chainlink.ts";
import { attestation } from "./prices.ts";
import { decodeObserve, meanTick, observeCall, tickPrice, type Twap, TWAPS } from "./twap.ts";

const TGAS = 10n ** 12n;
const EXPO = -8;

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: { board: { type: "string" }, vault: { type: "string" }, dry: { type: "boolean", default: false } },
});
if (!o.board && !o.dry) throw new Error("--board <board account> (or --dry to only read the pools)");
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
const chain = new Chain(rpc, o.board ?? "", keeperId, o.dry ? undefined : loadKey(network, keeperId));
const idOf = (t: Twap) => priceId(t.chain, t.pool);

let names = positionals;
if (o.vault) {
  const cfg = await chain.brainView<{ feeds: { contract: string; price_id: string; backup?: { contract?: string; price_id?: string } }[] } | null>(
    await chain.brainOf(o.vault), o.vault, "get_oracle");
  if (!cfg) throw new Error(`${o.vault} has no oracle set up`);
  const ids = cfg.feeds.flatMap((f) => [f.contract === o.board ? f.price_id : "", f.backup?.contract === o.board ? f.backup?.price_id ?? "" : ""]);
  names = Object.keys(TWAPS).filter((n) => ids.includes(idOf(TWAPS[n])));
}
if (!names.length) throw new Error(`which feeds? ${Object.keys(TWAPS).join(" ")} (or --vault <vault>)`);
for (const n of names) if (!TWAPS[n]) throw new Error(`unknown TWAP ${n}: ${Object.keys(TWAPS).join(" ")}`);

async function eth<T>(t: Twap, method: string, params: unknown[]): Promise<T> {
  const url = process.env[t.rpcEnv] ?? t.rpc;
  const res = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  const body = (await res.json()) as { result?: T; error?: { message: string } };
  if (body.error) throw new Error(`${url} ${method}: ${body.error.message}`);
  return body.result as T;
}

/** One pool, read at one pinned block: its tokens (checked), then observe([window, 0]). */
async function readTwap(name: string): Promise<{ id: string; price: bigint; blockTime: number }> {
  const t = TWAPS[name];
  const cid = Number(await eth<string>(t, "eth_chainId", []));
  if (cid !== t.evmChainId) throw new Error(`${name}: RPC is on chain ${cid}, not ${t.evmChainId}`);
  const blk = await eth<{ number: string; timestamp: string }>(t, "eth_getBlockByNumber", ["latest", false]);
  const call = (data: string) => eth<string>(t, "eth_call", [{ to: t.pool, data: "0x" + data }, blk.number]);
  const [t0, t1, obs] = await Promise.all([call("0dfe1681"), call("d21220a7"), call(observeCall(t.windowS))]);
  const addr = (w: string) => "0x" + w.slice(-40).toLowerCase();
  if (addr(t0) !== t.token0 || addr(t1) !== t.token1) throw new Error(`${name}: ${t.pool} holds ${addr(t0)} / ${addr(t1)}, expected ${t.token0} / ${t.token1}`);
  const [older, newer] = decodeObserve(obs);
  const tick = meanTick(older, newer, t.windowS);
  const price = tickPrice(tick, t, -EXPO);
  const [block, blockTime] = [Number(blk.number), Number(blk.timestamp)];
  console.log(`${name}: ${t.description} = ${Number(price) / 1e8} (${price} x 1e${EXPO})  chain ${t.chain} (evm ${t.evmChainId}) pool ${t.pool}`);
  console.log(`  source: block ${block} at ${blockTime}, tickCumulatives ${older} -> ${newer} over ${t.windowS} s, mean tick ${tick}`);
  return { id: idOf(t), price, blockTime };
}

if (!o.board) {
  for (const n of names) await readTwap(n);
  process.exit(0);
}
const source = await chain.view<{ kind: string; attesters?: string[]; k?: number }>(o.board, "get_source");
if (source.kind !== "attested") throw new Error(`${o.board} is a ${source.kind} board: a pool TWAP goes on an attested board (an Evm board can't check observe() yet)`);
const signer = KeyPair.fromString((process.env.ATTESTER_PRIVATE_KEY ?? loadKey(network, keeperId)) as KeyPairString);
const me = signer.getPublicKey().toString();
if (!source.attesters!.includes(me)) console.log(`warning: ${me} is not one of ${o.board}'s attesters`);
if (source.k! > 1) throw new Error(`${o.board} needs ${source.k} attesters: post with chainlink-price's --out/--with flow (not built for TWAPs)`);
for (const n of names) {
  const r = await readTwap(n);
  const sig = Buffer.from(signer.sign(attestation(r.id, r.price, EXPO, r.blockTime)).signature).toString("hex");
  const report = { attester: me, price: r.price.toString(), expo: EXPO, publish_time: r.blockTime, signature: sig };
  if (o.dry) { console.log(`${r.id}: would post ${JSON.stringify(report)}`); continue; }
  await chain.call(o.board, "push", { feed: r.id, reports: [report] }, 30n * TGAS);
  const p = await chain.view<{ price: string; expo: number; publish_time: number } | null>(o.board, "get_price_unsafe", { price_id: r.id });
  console.log(`${n} on ${o.board}: ${p ? `${p.price} x 1e${p.expo}, published ${p.publish_time}` : "none"}`);
}
