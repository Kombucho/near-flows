// Fresh Pyth prices for NEAR, so vaults' market floors have something to read.
//
//   npm run pyth -- --board <board> eth sol usdc   ATTEST them to our price board (the way that works today)
//   npm run pyth -- eth usdc                       push them to Pyth's own contract on NEAR
//   npm run pyth -- ... --vault <vault>            every feed that (relay) vault reads from there (its oracle config)
//   ... --dry                                      fetch and print, send nothing
//   ... --max-conf-bps 100                         board mode: don't sign a price Pyth itself is this unsure of (default 1%)
//
// Pyth is a pull oracle: prices are signed by Pyth's publishers, carried by
// Wormhole guardians, and anyone may submit them; the Pyth contract on NEAR
// checks the signatures. Submitting can't fake a price, only make it fresher.
// Since Aug 2026 Pyth's Hermes service needs an API key: PYTH_API_KEY.
//
// WHY the board mode: on 2026-11-06 pyth-oracle.testnet refused every Hermes update
// ("Source for attestation is not allowed"): Pyth's Aug 2026 Core upgrade moved the
// data source and NEAR's receiver wasn't updated. Until it is, the keeper relays
// Hermes's prices to an ATTESTED board under Pyth's own price ids. That TRUSTS THE
// KEEPER (k of the board's attesters) to relay them faithfully. Anyone can audit a
// price: the log gives the feed id and publish_time; Hermes serves the signed update
// for that exact time at /v2/updates/price/<publish_time>?ids[]=<id>. The board keeps
// the median of its attesters, so Pyth's own confidence isn't carried over: we refuse
// to sign a price whose confidence is wider than --max-conf-bps instead.
//
// Environment: PYTH_API_KEY, KEEPER_ACCOUNT_ID, ATTESTER_PRIVATE_KEY (board mode; default: the keeper's
// key), NEAR_NETWORK (default testnet), NEAR_RPC, PYTH_CONTRACT (default pyth-oracle.<network>),
// HERMES (default https://pyth.dourolabs.app/hermes).

import { parseArgs } from "node:util";
import { KeyPair, type KeyPairString } from "near-api-js";
import { Chain, loadKey } from "./chain.ts";
import { attestation, unsignable } from "./prices.ts";

const TGAS = 10n ** 12n;
// Pyth feed ids (hex, no 0x) for names used in the node library.
export const FEEDS: Record<string, string> = {
  eth: "ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace",
  btc: "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43",
  sol: "ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d",
  near: "c415de8d2eba7db216527dff4b60e8f3a5311c740dadb233e13e12547e226750",
  usdc: "eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a",
  nvda: "b1073854ed24cbc755dc527418f52b7d271f6cc967bbf8d8129112b18860a593",
};

type Parsed = { id: string; price: { price: string; expo: number; conf: string; publish_time: number } };

const { values: o, positionals } = parseArgs({
  allowPositionals: true,
  options: { vault: { type: "string" }, board: { type: "string" }, dry: { type: "boolean", default: false }, "max-conf-bps": { type: "string", default: "100" } },
});
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const pyth = process.env.PYTH_CONTRACT ?? `pyth-oracle.${network === "mainnet" ? "near" : "testnet"}`;
const target = o.board ?? pyth;
const hermes = process.env.HERMES ?? "https://pyth.dourolabs.app/hermes";
const apiKey = process.env.PYTH_API_KEY ?? "";
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
if (!apiKey) throw new Error("set PYTH_API_KEY (Hermes needs one since Aug 2026: https://docs.pyth.network/pro/pyth-core-upgrade)");
const chain = new Chain(rpc, target, keeperId, o.dry ? undefined : loadKey(network, keeperId));

let ids: string[];
if (o.vault) {
  const cfg = await chain.brainView<{ feeds: { name: string; contract: string; price_id: string }[] } | null>(await chain.brainOf(o.vault), o.vault, "get_oracle");
  if (!cfg) throw new Error(`${o.vault} has no oracle set up`);
  // On a board, only Pyth's ids are ours to relay (Chainlink feeds there are chainlink-price's).
  ids = cfg.feeds.filter((f) => f.contract === target).map((f) => f.price_id).filter((id) => !o.board || Object.values(FEEDS).includes(id));
} else {
  ids = positionals.map((n) => FEEDS[n] ?? n);
}
if (!ids.length) throw new Error("which feeds? names (eth usdc ...) or --vault <vault>");

const q = ids.map((id) => `ids[]=${id}`).join("&");
const res = await fetch(`${hermes}/v2/updates/price/latest?${q}&encoding=hex&parsed=true`, { headers: { authorization: `Bearer ${apiKey}` } });
if (!res.ok) throw new Error(`Hermes: HTTP ${res.status} ${await res.text()}`);
const body = (await res.json()) as { binary: { data: string[] }; parsed: Parsed[] };
for (const p of body.parsed) {
  const usd = Number(p.price.price) * 10 ** p.price.expo;
  console.log(`${p.id.slice(0, 8)} $${usd.toFixed(4)} ±${((Number(p.price.conf) / Number(p.price.price)) * 100).toFixed(3)}%  published ${p.price.publish_time} (${Math.round(Date.now() / 1000 - p.price.publish_time)} s ago)`);
}

if (o.board) {
  const source = await chain.view<{ kind: string; attesters?: string[]; k?: number }>(o.board, "get_source");
  if (source.kind !== "attested") throw new Error(`${o.board} is a ${source.kind} board: Pyth prices go to an attested one`);
  if (source.k! > 1) throw new Error(`${o.board} needs ${source.k} attesters per price: this relays one (combine reports by hand, as jupiter-price --with does)`);
  const signer = o.dry ? null : KeyPair.fromString((process.env.ATTESTER_PRIVATE_KEY ?? loadKey(network, keeperId)) as KeyPairString);
  const me = signer?.getPublicKey().toString();
  if (me && !source.attesters!.includes(me)) console.log(`warning: ${me} is not one of ${o.board}'s attesters`);
  for (const p of body.parsed) {
    const why = unsignable(p.price, Number(o["max-conf-bps"]));
    const audit = `${hermes}/v2/updates/price/${p.price.publish_time}?ids[]=${p.id}`;
    if (why) { console.log(`${p.id}: not signed: ${why}`); continue; }
    if (!signer) { console.log(`${p.id}: would attest ${p.price.price} x 1e${p.price.expo} at ${p.price.publish_time} (source: Hermes, audit: ${audit})`); continue; }
    const sig = signer.sign(attestation(p.id, BigInt(p.price.price), p.price.expo, p.price.publish_time)).signature;
    const report = { attester: me, price: p.price.price, expo: p.price.expo, publish_time: p.price.publish_time, signature: Buffer.from(sig).toString("hex") };
    await chain.call(o.board, "push", { feed: p.id, reports: [report] }, 30n * TGAS);
    console.log(`${p.id}: attested ${p.price.price} x 1e${p.price.expo} at ${p.price.publish_time} by ${me} (source: Hermes, audit: ${audit})`);
  }
  if (o.dry) process.exit(0);
  for (const id of ids) {
    const p = await chain.view<{ price: string; conf: string; expo: number; publish_time: number } | null>(o.board, "get_price_unsafe", { price_id: id });
    console.log(`${id.slice(0, 8)} on ${o.board}: ${p ? `${p.price} x 1e${p.expo}, published ${p.publish_time}` : "none"}`);
  }
  process.exit(0);
}

const data = body.binary.data[0];
const fee = BigInt(await chain.view<string>(pyth, "get_update_fee_estimate", { data }));
console.log(`update: ${data.length / 2} bytes, fee ${Number(fee) / 1e24} NEAR`);
if (o.dry) process.exit(0);
await chain.call(pyth, "update_price_feeds", { data }, 300n * TGAS, fee);
for (const id of ids) {
  const p = await chain.view<{ price: string; expo: number } | null>(pyth, "get_price", { price_identifier: id });
  console.log(`${id.slice(0, 8)} on ${pyth}: ${p ? Number(p.price) * 10 ** p.expo : "none"}`);
}
