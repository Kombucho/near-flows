// Push fresh Pyth prices to NEAR, so vaults' market floors have something to read.
//
//   npm run pyth -- eth usdc near        push those feeds (names from the vault's oracle config)
//   npm run pyth -- --vault <vault>      push every feed that (relay) vault uses: its oracle config, read from its brain
//   ... --dry                            fetch and estimate the fee, send nothing
//
// Pyth is a pull oracle: prices are signed by Pyth's publishers, carried by
// Wormhole guardians, and anyone may submit them; the Pyth contract on NEAR
// checks the signatures. Submitting can't fake a price, only make it fresher.
// Since Aug 2026 Pyth's Hermes service needs an API key: PYTH_API_KEY.
//
// Environment: PYTH_API_KEY, KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet), NEAR_RPC,
// PYTH_CONTRACT (default pyth-oracle.<network>), HERMES (default https://pyth.dourolabs.app/hermes).

import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";

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

const { values: o, positionals } = parseArgs({ allowPositionals: true, options: { vault: { type: "string" }, dry: { type: "boolean", default: false } } });
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const pyth = process.env.PYTH_CONTRACT ?? `pyth-oracle.${network === "mainnet" ? "near" : "testnet"}`;
const hermes = process.env.HERMES ?? "https://pyth.dourolabs.app/hermes";
const apiKey = process.env.PYTH_API_KEY ?? "";
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
if (!apiKey) throw new Error("set PYTH_API_KEY (Hermes needs one since Aug 2026: https://docs.pyth.network/pro/pyth-core-upgrade)");
const chain = new Chain(rpc, pyth, keeperId, o.dry ? undefined : loadKey(network, keeperId));

let ids: string[];
if (o.vault) {
  const cfg = await chain.brainView<{ feeds: { name: string; contract: string; price_id: string }[] } | null>(await chain.brainOf(o.vault), o.vault, "get_oracle");
  if (!cfg) throw new Error(`${o.vault} has no oracle set up`);
  ids = cfg.feeds.filter((f) => f.contract === pyth).map((f) => f.price_id);
} else {
  ids = positionals.map((n) => FEEDS[n] ?? n);
}
if (!ids.length) throw new Error("which feeds? names (eth usdc ...) or --vault <vault>");

const q = ids.map((id) => `ids[]=${id}`).join("&");
const res = await fetch(`${hermes}/v2/updates/price/latest?${q}&encoding=hex&parsed=true`, { headers: { authorization: `Bearer ${apiKey}` } });
if (!res.ok) throw new Error(`Hermes: HTTP ${res.status} ${await res.text()}`);
const body = (await res.json()) as { binary: { data: string[] }; parsed: { id: string; price: { price: string; expo: number; conf: string; publish_time: number } }[] };
for (const p of body.parsed) {
  const usd = Number(p.price.price) * 10 ** p.price.expo;
  console.log(`${p.id.slice(0, 8)} $${usd.toFixed(4)} ±${((Number(p.price.conf) / Number(p.price.price)) * 100).toFixed(3)}%  published ${Math.round(Date.now() / 1000 - p.price.publish_time)} s ago`);
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
