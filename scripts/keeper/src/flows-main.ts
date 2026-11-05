// Flow keeper: runs the steps of users' flows whose triggers have fired.
//
//   npm run flows                 dry run: prints what is due, sends nothing
//   npm run flows -- --send       runs due steps (as KEEPER_ACCOUNT_ID)
//   npm run flows -- --send --once
//
// It finds vaults through the factory, keeps the ones that named this keeper,
// and for each due step calls the vault's `step`. Price triggers are checked
// first with a free view, so the keeper doesn't pay for steps that would fail.
// The keeper can't do anything else to a vault: no withdrawing, no editing.
// Best practice: give it a key that may only call `step`.
//
// Environment: FACTORY_ID, KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC (default FastNEAR testnet), KEEPER_PRIVATE_KEY (or a near-cli key file).

import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { due, fired, priceOf, type Flow } from "./flows.ts";

const TGAS = 10n ** 12n;
const { values: opt } = parseArgs({
  options: {
    tick: { type: "string", default: "30" }, // seconds between rounds
    send: { type: "boolean", default: false },
    once: { type: "boolean", default: false },
  },
});

const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com";
const factory = process.env.FACTORY_ID ?? "";
const keeperId = process.env.KEEPER_ACCOUNT_ID ?? "";
if (!factory || !keeperId) throw new Error("set FACTORY_ID and KEEPER_ACCOUNT_ID");
const chain = new Chain(rpc, factory, keeperId, opt.send ? loadKey(network, keeperId) : undefined);
const log = (s: string) => console.log(`${new Date().toISOString().slice(11, 19)} ${s}`);

async function myVaults(): Promise<string[]> {
  const n = await chain.view<number>(factory, "get_vault_count");
  const all: string[] = [];
  for (let i = 0; i < n; i += 100) all.push(...(await chain.view<string[]>(factory, "get_vaults", { from_index: i, limit: 100 })));
  const mine: string[] = [];
  for (const v of all) if ((await chain.view<string | null>(v, "get_keeper")) === keeperId) mine.push(v);
  return mine;
}

async function round() {
  const vaults = await myVaults();
  const now = BigInt(Date.now()) * 1_000_000n;
  let ran = 0;
  for (const vault of vaults) {
    if (await chain.view(vault, "get_busy")) continue; // one step at a time per vault
    const ids = await chain.view<[number, string][]>(vault, "get_flows");
    const flows: [number, Flow][] = [];
    for (const [id] of ids) flows.push([id, await chain.view<Flow>(vault, "get_flow", { flow_id: id })]);
    for (const d of due(flows, now)) {
      if (d.price) {
        const s = d.price.source;
        const raw = await chain.view<string>(s.contract, s.method, JSON.parse(s.args)).catch(() => null);
        if (raw === null || !fired(d, priceOf(raw, s.per))) continue;
      }
      log(`${vault} flow ${d.flow} node ${d.node} "${d.label}" is due`);
      if (!opt.send) continue;
      try {
        await chain.call(vault, "step", { flow_id: d.flow, node: d.node }, 300n * TGAS);
        ran++;
        break; // the vault is busy until this step finishes: next vault
      } catch (e) {
        log(`  step failed: ${(e as Error).message.slice(0, 160)}`);
      }
    }
  }
  log(`${vaults.length} vault(s) watched, ${ran} step(s) sent${opt.send ? "" : " (dry run)"}`);
}

for (;;) {
  await round().catch((e) => log(`round failed: ${(e as Error).message}`));
  if (opt.once) break;
  await new Promise((r) => setTimeout(r, Number(opt.tick) * 1000));
}
