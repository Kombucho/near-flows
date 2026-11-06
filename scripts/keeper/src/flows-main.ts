// Flow keeper: runs the steps of users' flows whose triggers have fired.
//
//   npm run flows                 dry run: prints what is due, sends nothing
//   npm run flows -- --send       runs due steps (as KEEPER_ACCOUNT_ID)
//   npm run flows -- --send --once
//
// Vaults are relay vaults: it finds them through the relay factory, reads each
// one's record from the shared brain (one `get_vault` view: keeper, busy, flows),
// keeps the ones that named this keeper, and for each due step calls the
// vault's `x` door with {"step": ...}. A hidden flow's steps go as
// {"step_hidden": ...}, from the plan its owner shared in ~/.flows/hidden/<vault>.json
// (flows it has no plan for are skipped: the keeper can't see them). Price
// triggers are checked first with a free view, so the keeper doesn't pay for
// steps that would fail. Remote nodes (other chains) are only reported here:
// their transaction is built by the chain's own runner (EVM nonce and fees, a
// Solana message), which then calls step_remote and remote_report.
// The keeper can't do anything else to a vault: no withdrawing, no editing.
// Best practice: give it a key that may only call `x` on the vaults.
//
// Environment: FACTORY_ID (the relay factory), KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC (default FastNEAR testnet), KEEPER_PRIVATE_KEY (or a near-cli key file).

import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { due, fired, priceOf, runnable, type Flow, type FlowNode, type VaultState } from "./flows.ts";
import { loadStore, unlock, type Node } from "./hidden.ts";

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
const brain = await chain.view<string>(factory, "get_brain");
const log = (s: string) => console.log(`${new Date().toISOString().slice(11, 19)} ${s}`);

async function allVaults(): Promise<string[]> {
  const n = await chain.view<number>(factory, "get_vault_count");
  const all: string[] = [];
  for (let i = 0; i < n; i += 100) all.push(...(await chain.view<string[]>(factory, "get_vaults", { from_index: i, limit: 100 })));
  return all;
}

async function round() {
  const vaults = await allVaults();
  const now = BigInt(Date.now()) * 1_000_000n;
  let mine = 0;
  let ran = 0;
  for (const vault of vaults) {
    const state = await chain.brainView<VaultState | null>(brain, vault, "get_vault");
    if (state?.keeper === keeperId) mine++;
    const flows: [number, Flow][] = [];
    const plans = loadStore(vault);
    const hidden = new Map<number, { nodes: Node[]; salts: string[] }>();
    for (const [id, f] of runnable(state, keeperId)) {
      if (f.hidden?.length) {
        const nodes = unlock(plans[id], f.hidden);
        if (!nodes) continue; // not ours to see, or the plan is stale
        hidden.set(id, plans[id]);
        f.nodes = nodes as unknown as FlowNode[];
      }
      flows.push([id, f]);
    }
    for (const d of due(flows, now)) {
      if (d.remote) {
        log(`${vault} flow ${d.flow} node ${d.node} "${d.label}" is due on another chain: its runner sends step_remote`);
        continue;
      }
      if (d.price) {
        const s = d.price.source;
        const raw = await chain.view<string>(s.contract, s.method, JSON.parse(s.args)).catch(() => null);
        if (raw === null || !fired(d, priceOf(raw, s.per))) continue;
      }
      // The owner's price for this keeper's run (paid by the vault if the step works).
      const fee = BigInt(state?.keeper_fee ?? "0");
      log(`${vault} flow ${d.flow} node ${d.node} "${d.label}" is due${fee > 0n ? ` (pays ${Number(fee) / 1e24} NEAR)` : ""}`);
      if (!opt.send) continue;
      try {
        const h = hidden.get(d.flow);
        if (h) await chain.vaultCall(vault, "step_hidden", { flow_id: d.flow, node: d.node, reveal: h.nodes[d.node], salt: h.salts[d.node] });
        else await chain.vaultCall(vault, "step", { flow_id: d.flow, node: d.node });
        ran++;
        break; // the vault is busy until this step finishes: next vault
      } catch (e) {
        log(`  step failed: ${(e as Error).message.slice(0, 160)}`);
      }
    }
  }
  log(`${mine} of ${vaults.length} vault(s) name this keeper, ${ran} step(s) sent${opt.send ? "" : " (dry run)"}`);
}

for (;;) {
  await round().catch((e) => log(`round failed: ${(e as Error).message}`));
  if (opt.once) break;
  await new Promise((r) => setTimeout(r, Number(opt.tick) * 1000));
}
