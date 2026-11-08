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
// A vault paying this keeper (keeper_fee > 0) whose free NEAR can't cover the
// price is skipped (the brain would refuse it): its owner can top up, or run
// their own keeper for free (AGENTS.md).
//
// Prices (--board <board>): just before a step goes, the keeper posts fresh prices
// for the feeds that vault reads from the board (Chainlink feeds always, Pyth's if
// PYTH_API_KEY is set), so its market floor reads a price seconds old. A price
// trigger that reads the board gets one post before it is checked. Besides that,
// every quarter of the board's averaging window (150 s for 600 s) it posts them for
// each vault that names this keeper and has a flow: WHY the board serves a new feed's
// average (a vault's `ema` backup) only on half a window of unbroken history, and any
// one price moves it a third of the way at most, so a lone on-demand price can't be
// its own backup (contracts/priceboard `store`). Without these posts a real move
// between steps would outrun the average and the vault's floor would refuse.
// A vault with no flows costs nothing.
//
// Environment: FACTORY_ID (the relay factory), KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC (default FastNEAR testnet), KEEPER_PRIVATE_KEY (or a near-cli key file), PYTH_API_KEY.

import { execFile } from "node:child_process";
import { parseArgs, promisify } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { canPayKeeper, due, fired, keeperUnpaid, priceOf, runnable, warmDue, type Flow, type FlowNode, type VaultState } from "./flows.ts";
import { loadStore, unlock, type Node } from "./hidden.ts";

const { values: opt } = parseArgs({
  options: {
    tick: { type: "string", default: "30" }, // seconds between rounds
    send: { type: "boolean", default: false },
    once: { type: "boolean", default: false },
    board: { type: "string" }, // post this board's prices on demand
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

// Feeds posted for a vault this round (one post per vault per round is enough).
let fresh = new Set<string>();
// When each vault's board prices last went, and the board's averaging window (s).
const posted = new Map<string, number>();
const windowS = opt.board ? await chain.view<number>(opt.board, "get_ema_window_s").catch(() => 600) : 600;
const run = promisify(execFile);
/** Post fresh board prices for every feed `vault` reads from the board. Never throws:
 *  a vault with no board feeds, or a source that's down, just gets no post (the
 *  vault then refuses a stale price itself). */
async function freshen(vault: string) {
  if (!opt.board || !opt.send || fresh.has(vault)) return;
  fresh.add(vault);
  posted.set(vault, Date.now());
  const scripts = ["chainlink-price", ...(process.env.PYTH_API_KEY ? ["pyth"] : [])];
  for (const s of scripts) {
    try {
      const { stdout } = await run("npm", ["run", "-s", s, "--", "--board", opt.board, "--vault", vault], { timeout: 60_000 });
      for (const l of stdout.split("\n")) if (l.includes(` on ${opt.board}: `)) log(`  ${s}: ${l.trim()}`);
    } catch (e) {
      const m = (e as { stderr?: string }).stderr ?? (e as Error).message;
      if (!/which feeds\?|has no oracle/.test(m)) log(`  ${s} for ${vault} failed: ${m.split("\n").find((l) => /Error/.test(l))?.trim() ?? m.slice(0, 160)}`);
    }
  }
}

async function allVaults(): Promise<string[]> {
  const n = await chain.view<number>(factory, "get_vault_count");
  const all: string[] = [];
  for (let i = 0; i < n; i += 100) all.push(...(await chain.view<string[]>(factory, "get_vaults", { from_index: i, limit: 100 })));
  return all;
}

async function round() {
  fresh = new Set();
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
    // Keep the board's averages fed for vaults that may step (see the header).
    if (state?.keeper === keeperId && flows.length && warmDue(posted.get(vault), Date.now(), windowS)) await freshen(vault);
    const todo = due(flows, now);
    // The owner's price for this keeper's runs (paid by the vault if a step works).
    // A vault that can't pay it gets no free work: the brain would refuse the step.
    const fee = BigInt(state?.keeper_fee ?? "0");
    if (fee > 0n && todo.some((d) => !d.remote)) {
      const free = await chain.freeNear(brain, vault).catch(() => null);
      if (free !== null && !canPayKeeper(fee, free)) {
        log(keeperUnpaid(vault, fee, free));
        continue;
      }
    }
    for (const d of todo) {
      if (d.remote) {
        log(`${vault} flow ${d.flow} node ${d.node} "${d.label}" is due on another chain: its runner sends step_remote`);
        continue;
      }
      if (d.price) {
        const s = d.price.source;
        if (s.contract === opt.board) await freshen(vault);
        const raw = await chain.view<string>(s.contract, s.method, JSON.parse(s.args)).catch(() => null);
        if (raw === null || !fired(d, priceOf(raw, s.per))) continue;
      }
      log(`${vault} flow ${d.flow} node ${d.node} "${d.label}" is due${fee > 0n ? ` (pays ${Number(fee) / 1e24} NEAR)` : ""}`);
      if (!opt.send) continue;
      await freshen(vault);
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
