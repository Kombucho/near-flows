// Post Chainlink prices from other chains (NEAR/USD on Arbitrum, NVDA on Robinhood
// Chain) to a price board (contracts/priceboard), so vaults' market floors and
// price triggers have them where our Pyth key isn't entitled.
//
//   npm run chainlink-price -- --board <board> near nvda       read, sign (or query), post
//   npm run chainlink-price -- --board <board> --vault <vault>  every feed that vault reads from the board
//   ... --dry                         read and print, send nothing (without --board: just read, no NEAR account needed)
//   ... --out mine.json               attested board: only sign, write this attester's reports
//   ... --with a.json --with b.json   attested board: add other attesters' reports (k of n)
//
// What the board is decides how the price gets there:
// - ATTESTED board (works today, no keys but the keeper's): we read the feed with a
//   plain eth_call on a public RPC and sign what we read. The board TRUSTS US (k of
//   its attesters) for that. Anyone can audit a price: the log gives the chain, the
//   feed, the block, the round id and updatedAt; replay the eth_call at that block.
// - EVM board (trustless): we ask Wormhole's guardians to make the same eth_call and
//   post their signed answer; the board checks the signatures itself. Needs
//   WORMHOLE_QUERY_API_KEY (the query proxy is allow-listed).
//
// We only sign what the board would take: answer > 0, at most the feed's heartbeat
// old at the block. publish_time is the block's time (the feed vouches the price is
// within its deviation threshold of the answer at that block).
//
// Environment: KEEPER_ACCOUNT_ID, ATTESTER_PRIVATE_KEY (default: the keeper's key), NEAR_NETWORK
// (default testnet), NEAR_RPC, ARBITRUM_RPC / OPTIMISM_RPC / ROBINHOOD_RPC (override the public RPCs),
// WORMHOLE_QUERY_API_KEY and QUERY_PROXY (Evm boards only).

import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { KeyPair, type KeyPairString } from "near-api-js";
import { Chain, loadKey } from "./chain.ts";
import { decodeDecimals, decodeRound, decodeString, DECIMALS, ethCallRequest, type Feed, feedCalls, FEEDS, LATEST_ROUND_DATA, phaseRound, priceId, stale } from "./chainlink.ts";
import { attestation } from "./prices.ts";

const TGAS = 10n ** 12n;
type Report = { attester: string; price: string; expo: number; publish_time: number; signature: string };

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: {
    board: { type: "string" }, vault: { type: "string" }, out: { type: "string" },
    with: { type: "string", multiple: true, default: [] }, dry: { type: "boolean", default: false },
  },
});
if (!o.board && !o.dry) throw new Error("--board <board account> (or --dry to only read the feeds)");
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
const chain = new Chain(rpc, o.board ?? "", keeperId, o.dry || o.out ? undefined : loadKey(network, keeperId));

// Which feeds: by name, or every board feed in the vault's oracle config.
let names = positionals;
if (o.vault) {
  const cfg = await chain.brainView<{ feeds: { name: string; contract: string; price_id: string; backup?: { kind: string; contract?: string; price_id?: string } }[] } | null>(
    await chain.brainOf(o.vault), o.vault, "get_oracle");
  if (!cfg) throw new Error(`${o.vault} has no oracle set up`);
  const ids = cfg.feeds.flatMap((f) => [f.contract === o.board ? f.price_id : "", f.backup?.contract === o.board ? f.backup?.price_id ?? "" : ""]).filter(Boolean);
  names = Object.keys(FEEDS).filter((n) => ids.includes(priceId(FEEDS[n].chain, FEEDS[n].address)));
  const unknown = ids.filter((id) => !names.some((n) => priceId(FEEDS[n].chain, FEEDS[n].address) === id));
  if (unknown.length) console.log(`not Chainlink feeds this script knows (skipped): ${unknown.join(", ")}`);
}
if (!names.length) throw new Error(`which feeds? ${Object.keys(FEEDS).join(" ")} (or --vault <vault>)`);
for (const n of names) if (!FEEDS[n]) throw new Error(`unknown feed ${n}: ${Object.keys(FEEDS).join(" ")}`);

async function eth<T>(f: Feed, method: string, params: unknown[]): Promise<T> {
  const url = process.env[f.rpcEnv] ?? f.rpc;
  const res = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }) });
  if (!res.ok) throw new Error(`${url}: HTTP ${res.status}`);
  const body = (await res.json()) as { result?: T; error?: { message: string } };
  if (body.error) throw new Error(`${url} ${method}: ${body.error.message}`);
  return body.result as T;
}

type Reading = { name: string; feed: Feed; id: string; block: number; blockTime: number; answer: bigint; decimals: number; roundId: bigint; updatedAt: number };

/** One feed, read at one pinned block (so the round, the decimals and the time all belong together). */
async function readFeed(name: string): Promise<Reading | null> {
  const f = FEEDS[name];
  const id = Number(await eth<string>(f, "eth_chainId", []));
  if (id !== f.evmChainId) throw new Error(`${name}: RPC is on chain ${id}, not ${f.evmChainId}`);
  const blk = await eth<{ number: string; timestamp: string }>(f, "eth_getBlockByNumber", ["latest", false]);
  const call = (data: string) => eth<string>(f, "eth_call", [{ to: f.address, data: "0x" + data }, blk.number]);
  const [desc, dec, rnd] = await Promise.all([call("7284e416"), call(DECIMALS), call(LATEST_ROUND_DATA)]);
  if (decodeString(desc) !== f.description) throw new Error(`${name}: ${f.address} says "${decodeString(desc)}", expected "${f.description}"`);
  const r = decodeRound(rnd);
  const [block, blockTime] = [Number(blk.number), Number(blk.timestamp)];
  const decimals = decodeDecimals(dec);
  const { phase, round } = phaseRound(r.roundId);
  console.log(`${name}: ${f.description} = ${Number(r.answer) / 10 ** decimals} (${r.answer} x 1e-${decimals})  chain ${f.chain} (evm ${f.evmChainId}) feed ${f.address}`);
  console.log(`  source: block ${block} at ${blockTime}, round ${r.roundId} (phase ${phase}, round ${round}), updatedAt ${r.updatedAt} (${blockTime - Number(r.updatedAt)} s before the block)`);
  const why = stale(r, blockTime, f.heartbeatS);
  if (why) { console.log(`  skipped: ${why}`); return null; }
  return { name, feed: f, id: priceId(f.chain, f.address), block, blockTime, answer: r.answer, decimals, roundId: r.roundId, updatedAt: Number(r.updatedAt) };
}

if (!o.board) {
  for (const n of names) await readFeed(n);
  process.exit(0);
}
const board = o.board;
const source = await chain.view<{ kind: string; attesters?: string[]; k?: number }>(board, "get_source");

if (source.kind === "evm") {
  // Trustless path: the guardians make the read, we only carry it.
  const apiKey = process.env.WORMHOLE_QUERY_API_KEY ?? "";
  if (!apiKey) throw new Error(`${o.board} is an Evm board: set WORMHOLE_QUERY_API_KEY (Wormhole's query proxy only serves allow-listed keys), or post to an attested board`);
  const proxy = process.env.QUERY_PROXY ?? "https://query.wormhole.com/v1/query";
  const byChain = new Map<number, Feed[]>();
  for (const n of names) byChain.set(FEEDS[n].chain, [...(byChain.get(FEEDS[n].chain) ?? []), FEEDS[n]]);
  const perChain = await Promise.all([...byChain].map(async ([c, fs]) => ({
    // A block number, not "latest": every guardian must read the same block.
    chain: c, block: (await eth<{ number: string }>(fs[0], "eth_getBlockByNumber", ["latest", false])).number, calls: fs.flatMap(feedCalls),
  })));
  const request = ethCallRequest(perChain);
  const res = await fetch(proxy, { method: "POST", headers: { "content-type": "application/json", "x-api-key": apiKey }, body: JSON.stringify({ bytes: Buffer.from(request).toString("hex") }) });
  if (!res.ok) throw new Error(`query proxy: HTTP ${res.status} ${await res.text()}`);
  const { bytes, signatures } = (await res.json()) as { bytes: string; signatures: string[] };
  console.log(`signed by ${signatures.length} guardians, ${bytes.length / 2} bytes, blocks ${perChain.map((q) => `${q.chain}:${Number(q.block)}`).join(" ")}`);
  if (o.dry) { console.log(JSON.stringify({ bytes, signatures })); process.exit(0); }
  await chain.call(o.board, "push_evm", { response: bytes, signatures }, 100n * TGAS);
} else if (source.kind === "attested") {
  // Keyless path (but trusted): we read, we sign.
  const signer = KeyPair.fromString((process.env.ATTESTER_PRIVATE_KEY ?? loadKey(network, keeperId)) as KeyPairString);
  const me = signer.getPublicKey().toString();
  if (!source.attesters!.includes(me)) console.log(`warning: ${me} is not one of ${o.board}'s attesters`);
  const mine: Record<string, Report> = {};
  for (const n of names) {
    const r = await readFeed(n);
    if (!r) continue;
    const expo = -r.decimals;
    const sig = signer.sign(attestation(r.id, r.answer, expo, r.blockTime)).signature;
    mine[r.id] = { attester: me, price: r.answer.toString(), expo, publish_time: r.blockTime, signature: Buffer.from(sig).toString("hex") };
    console.log(`  price id ${r.id}, signed by ${me}`);
  }
  if (o.out) {
    writeFileSync(o.out, JSON.stringify(mine, null, 1));
    console.log(`wrote ${Object.keys(mine).length} reports to ${o.out}`);
    process.exit(0);
  }
  const others = o.with.map((f) => JSON.parse(readFileSync(f, "utf8")) as Record<string, Report>);
  for (const [id, r] of Object.entries(mine)) {
    const reports = [r, ...others.map((x) => x[id]).filter((x): x is Report => !!x && x.attester !== me)];
    if (reports.length < source.k!) { console.log(`${id}: ${reports.length} of ${source.k} reports: not posted`); continue; }
    if (o.dry) { console.log(`${id}: would post ${reports.length} report(s)`); continue; }
    await chain.call(o.board, "push", { feed: id, reports }, 30n * TGAS);
  }
} else {
  throw new Error(`${o.board} is a ${source.kind} board: it takes pump.fun curves, not Chainlink feeds`);
}
if (o.dry) process.exit(0);
for (const n of names) {
  const id = priceId(FEEDS[n].chain, FEEDS[n].address);
  const p = await chain.view<{ price: string; conf: string; expo: number; publish_time: number } | null>(o.board, "get_price_unsafe", { price_id: id });
  console.log(`${n} on ${o.board}: ${p ? `${p.price} x 1e${p.expo} ±${p.conf}, published ${p.publish_time}` : "none"}`);
}
