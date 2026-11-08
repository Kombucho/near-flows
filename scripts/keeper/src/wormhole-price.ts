// Ask Wormhole's guardians to read pump.fun bonding curves on Solana and post
// their signed answer to a WORMHOLE price board (contracts/priceboard).
//
//   npm run wormhole-price -- --board <board> <mint>...     query and post
//   ... --dry                                               query, print, send nothing
//
// Wormhole Queries: each guardian reads the account at PDA ["bonding-curve", mint]
// of the pump program (finalized) and signs the result; the query proxy collects
// a quorum. The board checks the guardian signatures itself, so this helper is
// trusted with nothing: it can only post a true reading, or none. The board
// prices the coin as virtual quote / virtual token, then in USD with Pyth.
// Coins that finished their curve (moved to an AMM pool) are refused.
// What it proves is the curve's state at the slot read, which anyone can move by
// buying into it: a true reading, not a trusted price. So it is for price triggers
// only; a vault names the feed single_source and no market floor reads it.
//
// Environment: WORMHOLE_QUERY_API_KEY (required: the proxy is allow-listed; ask Wormhole for a key),
// QUERY_PROXY (default https://query.wormhole.com/v1/query; testnet: https://testnet.query.wormhole.com/v1/query),
// KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet), NEAR_RPC.

import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { curveQuery, EXPO, feedId } from "./prices.ts";

const TGAS = 10n ** 12n;
const NEW_FEED = 5n * 10n ** 21n; // 0.005 NEAR per coin the board hasn't seen (its storage)

const { values: o, positionals: mints } = parseArgs({ allowPositionals: true, options: { board: { type: "string" }, dry: { type: "boolean", default: false } } });
const apiKey = process.env.WORMHOLE_QUERY_API_KEY ?? "";
if (!apiKey) throw new Error("set WORMHOLE_QUERY_API_KEY (Wormhole's query proxy only serves allow-listed keys)");
if (!mints.length) throw new Error("which coins? give their Solana mints");
if (!o.board && !o.dry) throw new Error("--board <board account>");
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const proxy = process.env.QUERY_PROXY ?? "https://query.wormhole.com/v1/query";
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";

const request = curveQuery(mints);
const res = await fetch(proxy, { method: "POST", headers: { "content-type": "application/json", "x-api-key": apiKey }, body: JSON.stringify({ bytes: Buffer.from(request).toString("hex") }) });
if (!res.ok) throw new Error(`query proxy: HTTP ${res.status} ${await res.text()}`);
const { bytes, signatures } = (await res.json()) as { bytes: string; signatures: string[] };
console.log(`signed by ${signatures.length} guardians, ${bytes.length / 2} bytes`);
if (o.dry || !o.board) { console.log(JSON.stringify({ bytes, signatures })); process.exit(0); }

const chain = new Chain(rpc, o.board, keeperId, loadKey(network, keeperId));
let fresh = 0n;
for (const m of mints) if (!(await chain.view(o.board, "get_price_unsafe", { price_id: feedId(m) }))) fresh += 1n;
await chain.call(o.board, "push_query", { response: bytes, signatures }, 150n * TGAS, fresh * NEW_FEED);
for (const m of mints) {
  const p = await chain.view<{ price: string; conf: string; publish_time: number } | null>(o.board, "get_price_unsafe", { price_id: feedId(m) });
  console.log(`${m} on ${o.board}: ${p ? `${p.price} x 1e${EXPO} ±${p.conf}, published ${p.publish_time}` : "none"}`);
}
