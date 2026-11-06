// Attest Jupiter's price for memecoins and post it to an ATTESTED price board
// (contracts/priceboard), so vaults' market floors can follow coins Pyth doesn't list.
//
//   npm run jupiter-price -- --board <board> <mint>...       sign and post (needs k = 1, or --with)
//   ... --out mine.json                  only sign: write this attester's reports, for someone to combine
//   ... --with a.json --with b.json      add other attesters' reports, then post (k of n)
//   ... --dry                            fetch and sign, send nothing
//   ... --min-liquidity 1000             refuse coins with less USD liquidity on Jupiter (default 1000)
//
// TRUSTS THE ATTESTERS. Jupiter's price API is off-chain and unsigned: no
// contract can check it, so the board believes whatever k of its n attesters
// sign. Several attesters on separate machines (and the board's median) limit
// one bad or hacked key; nothing limits k colluding ones. For a check that
// trusts Wormhole's guardians instead, see wormhole-price.ts, and set one board
// as the other's backup in the vault's oracle config.
// A thin coin's price is cheap to move: below --min-liquidity we don't sign at all.
//
// Environment: JUPITER_API_KEY (required; portal.jup.ag), ATTESTER_PRIVATE_KEY (ed25519:..., the
// attester's own key; default: the keeper's key), KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC, JUPITER_PRICE_API (default https://api.jup.ag/price/v3).

import { readFileSync, writeFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { KeyPair, type KeyPairString } from "near-api-js";
import { Chain, loadKey } from "./chain.ts";
import { attestation, EXPO, feedId, toUnits } from "./prices.ts";

const TGAS = 10n ** 12n;
type Report = { attester: string; price: string; expo: number; publish_time: number; signature: string };

const { values: o, positionals: mints } = parseArgs({
  allowPositionals: true,
  options: {
    board: { type: "string" }, out: { type: "string" }, with: { type: "string", multiple: true, default: [] },
    dry: { type: "boolean", default: false }, "min-liquidity": { type: "string", default: "1000" },
  },
});
const apiKey = process.env.JUPITER_API_KEY ?? "";
if (!apiKey) throw new Error("set JUPITER_API_KEY (Jupiter's price API needs one: https://portal.jup.ag)");
if (!mints.length) throw new Error("which coins? give their Solana mints");
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? (network === "mainnet" ? "https://rpc.mainnet.fastnear.com" : "https://test.rpc.fastnear.com");
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
const signer = KeyPair.fromString((process.env.ATTESTER_PRIVATE_KEY ?? loadKey(network, keeperId)) as KeyPairString);
const me = signer.getPublicKey().toString();
const api = process.env.JUPITER_PRICE_API ?? "https://api.jup.ag/price/v3";

const res = await fetch(`${api}?ids=${mints.join(",")}`, { headers: { "x-api-key": apiKey } });
if (!res.ok) throw new Error(`Jupiter: HTTP ${res.status} ${await res.text()}`);
const prices = (await res.json()) as Record<string, { usdPrice: number; liquidity: number; blockId: number } | null>;
const now = Math.floor(Date.now() / 1000); // Jupiter's answer carries a slot, not a time: our clock is the publish time

const mine: Record<string, Report> = {};
for (const mint of mints) {
  const p = prices[mint];
  if (!p) { console.log(`${mint}: Jupiter has no price (it leaves out tokens it can't price reliably): skipped`); continue; }
  if (p.liquidity < Number(o["min-liquidity"])) { console.log(`${mint}: $${p.liquidity.toFixed(0)} liquidity < ${o["min-liquidity"]}: skipped`); continue; }
  const units = toUnits(p.usdPrice);
  if (units === 0n) { console.log(`${mint}: $${p.usdPrice} is below 1e${EXPO}: skipped`); continue; }
  const sig = signer.sign(attestation(feedId(mint), units, EXPO, now)).signature;
  mine[mint] = { attester: me, price: units.toString(), expo: EXPO, publish_time: now, signature: Buffer.from(sig).toString("hex") };
  console.log(`${mint}: $${p.usdPrice} (liquidity $${p.liquidity.toFixed(0)}, slot ${p.blockId}) -> ${units} x 1e${EXPO}, signed by ${me}`);
}

if (o.out) {
  writeFileSync(o.out, JSON.stringify(mine, null, 1));
  console.log(`wrote ${Object.keys(mine).length} reports to ${o.out}`);
  process.exit(0);
}
if (!o.board) throw new Error("--board <board account> (or --out <file> to only sign)");
const others = o.with.map((f) => JSON.parse(readFileSync(f, "utf8")) as Record<string, Report>);
const chain = new Chain(rpc, o.board, keeperId, o.dry ? undefined : loadKey(network, keeperId));
const source = await chain.view<{ kind: string; attesters?: string[]; k?: number }>(o.board, "get_source");
if (source.kind !== "attested") throw new Error(`${o.board} is a ${source.kind} board, not an attested one`);
if (!source.attesters!.includes(me)) console.log(`warning: ${me} is not one of ${o.board}'s attesters`);

for (const [mint, r] of Object.entries(mine)) {
  const reports = [r, ...others.map((x) => x[mint]).filter((x): x is Report => !!x && x.attester !== me)];
  if (reports.length < source.k!) { console.log(`${mint}: ${reports.length} of ${source.k} reports: not posted`); continue; }
  if (o.dry) { console.log(`${mint}: would post ${reports.length} reports`); continue; }
  await chain.call(o.board, "push", { feed: feedId(mint), reports }, 30n * TGAS);
  const p = await chain.view<{ price: string; conf: string; publish_time: number } | null>(o.board, "get_price_unsafe", { price_id: feedId(mint) });
  console.log(`${mint} on ${o.board}: ${p ? `${p.price} x 1e${EXPO} ±${p.conf}, published ${p.publish_time}` : "none"}`);
}
