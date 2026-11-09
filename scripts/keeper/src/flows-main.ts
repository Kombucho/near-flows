// Flow keeper: runs the steps of users' flows whose triggers have fired.
//
//   npm run flows                 dry run: prints what is due, sends nothing
//   npm run flows -- --send       runs due steps (as KEEPER_ACCOUNT_ID)
//   npm run flows -- --send --once
//   npm run flows -- --send --remote   also runs remote nodes: other chains and hops (below)
//
// Vaults are relay vaults: it finds them through the relay factory, reads each
// one's record from the shared brain (one `get_vault` view: keeper, busy, flows),
// keeps the ones that named this keeper, and for each due step calls the
// vault's `x` door with {"step": ...}. A hidden flow's steps go as
// {"step_hidden": ...}, from the plan its owner shared in ~/.flows/hidden/<vault>.json
// (flows it has no plan for are skipped: the keeper can't see them). Price
// triggers are checked first with a free view, so the keeper doesn't pay for
// steps that would fail.
//
// Remote nodes (--send --remote, remote-run.ts): a node whose action is a transaction
// on another chain (an EVM or Solana template) or a hop out of the vault's Intents
// account. The keeper builds what the brain leaves open (a quoted minimum, a Solana
// message from the protocol's SDK; the nonce, fee cap and deadline are the brain's),
// calls step_remote, broadcasts the signed transaction to that chain's RPC, measures
// what landed at the vault's own address there and calls remote_report with it. A hop
// is sent by NEAR itself; the keeper watches the vault's balance where it lands.
// Without --remote, or without the chain's RPC (EVM_RPC_<chainid>, SOLANA_RPC), they
// are only listed: nothing leaves NEAR. A template with no builder is skipped, never
// guessed. A step that can't land, or can't be seen to, is left to the owner (only
// the owner may report one failed). In-flight steps are kept in
// ~/.flows/keeper/<keeper>.json (FLOWS_KEEPER_DIR), so a restart resumes them.
// The keeper can't do anything else to a vault: no withdrawing, no editing.
// Best practice: give it a key that may only call `x` on the vaults.
// A vault paying this keeper (keeper_fee > 0) whose free NEAR can't cover the
// price is skipped (the brain would refuse it): its owner can top up, or run
// their own keeper for free (AGENTS.md).
//
// Prices (--board <board>[,<board>...]): ON DEMAND only. Just before a step goes, the keeper
// posts fresh prices for the feeds that vault reads from those boards (a pyth_core board
// gets Pyth's signed update as is, which it checks itself), each feed's
// price AND its backup (a different publisher: Chainlink for a Pyth price, Chainlink
// on another chain for Chainlink), so its market floor reads prices seconds old that
// the vault checks against each other. Chainlink feeds always, Pyth's if
// PYTH_API_KEY is set. A price trigger that reads the board gets one post before
// it is checked. Nothing is posted between steps: the board keeps no average, so
// there is nothing to keep warm, and a vault with nothing due costs nothing.
//
// Environment: FACTORY_ID (the relay factory), KEEPER_ACCOUNT_ID, NEAR_NETWORK (default testnet),
// NEAR_RPC (default FastNEAR testnet), KEEPER_PRIVATE_KEY (or a near-cli key file), PYTH_API_KEY.
// Remote nodes: EVM_RPC_<chainid>, SOLANA_RPC, FLOWS_KEEPER_DIR, REMOTE_SLIPPAGE_BPS (100),
// SOL_SLIPPAGE_PCT (5), REMOTE_WAIT_MIN (30), HOP_WAIT_MIN (180), HOP_BRIDGE (1 on mainnet), V4_QUOTER_<chainid>.

import { execFile } from "node:child_process";
import { parseArgs, promisify } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import { canPayKeeper, due, fired, keeperUnpaid, priceOf, runnable, type Flow, type FlowNode, type VaultState } from "./flows.ts";
import { loadStore, unlock, type Node } from "./hidden.ts";
import { evmPlan, solPlan, statePath, type Remote } from "./remote.ts";
import { optsFromEnv, RemoteRunner, type RemoteNode, type VaultFull } from "./remote-run.ts";

const { values: opt } = parseArgs({
  options: {
    tick: { type: "string", default: "30" }, // seconds between rounds
    send: { type: "boolean", default: false },
    once: { type: "boolean", default: false },
    board: { type: "string" }, // post these boards' prices on demand (comma list: e.g. the pyth_core board and the attested one)
    remote: { type: "boolean", default: false }, // with --send: also run remote nodes (other chains, hops)
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
const boards = (opt.board ?? "").split(",").map((b) => b.trim()).filter(Boolean);
// Remote nodes leave NEAR only with both flags: --send (sign) and --remote (other chains).
const runner = opt.send && opt.remote ? new RemoteRunner(chain, brain, optsFromEnv(statePath(keeperId), log)) : null;
if (runner?.inflight.length) log(`resuming ${runner.inflight.length} remote step(s) in flight (${statePath(keeperId)})`);

/** One line on what a remote node needs, for the dry run (no network). */
function remoteNote(r: Remote, state: VaultFull): string {
  const c = r.call;
  if (c.chain === "hop") return `a hop to ${r.out}`;
  const p = c.chain === "evm" ? evmPlan(state.evm, c) : solPlan(state.sol, c, 0n);
  return "skip" in p ? `${c.template}: ${p.skip}` : `${c.chain} ${c.template}`;
}

// Feeds posted for a vault this round (one post per vault per round is enough).
let fresh = new Set<string>();
const run = promisify(execFile);
/** Post fresh board prices for every feed `vault` reads from the board. Never throws:
 *  a vault with no board feeds, or a source that's down, just gets no post (the
 *  vault then refuses a stale price itself). */
async function freshen(vault: string) {
  if (!boards.length || !opt.send || fresh.has(vault)) return;
  fresh.add(vault);
  const scripts = ["chainlink-price", ...(process.env.PYTH_API_KEY ? ["pyth"] : [])];
  // Every board x every script: each script posts only what it can to that kind of board
  // (and says "which feeds?" when there's nothing, which is fine).
  for (const board of boards) {
    for (const s of scripts) {
      try {
        const { stdout } = await run("npm", ["run", "-s", s, "--", "--board", board, "--vault", vault], { timeout: 60_000 });
        for (const l of stdout.split("\n")) if (l.includes(` on ${board}: `)) log(`  ${s}: ${l.trim()}`);
      } catch (e) {
        const m = (e as { stderr?: string }).stderr ?? (e as Error).message;
        // "not newer": another vault's post this second already put the same price there.
        if (!/which feeds\?|has no oracle|not newer than the board/.test(m)) log(`  ${s} for ${vault} on ${board} failed: ${m.split("\n").find((l) => /Error/.test(l))?.trim() ?? m.slice(0, 160)}`);
      }
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
  const states = new Map<string, (VaultState & VaultFull) | null>();
  for (const vault of vaults) states.set(vault, await chain.brainView<(VaultState & VaultFull) | null>(brain, vault, "get_vault"));
  if (runner) {
    // In-flight remote steps first: a report frees its flow and feeds the next node.
    const ours = new Map([...states].filter(([, s]) => s?.keeper === keeperId));
    const before = runner.inflight.length;
    await runner.follow(ours);
    if (runner.inflight.length !== before) for (const v of ours.keys()) states.set(v, await chain.brainView(brain, v, "get_vault"));
  }
  for (const vault of vaults) {
    const state = states.get(vault) ?? null;
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
    const todo = due(flows, now);
    // The owner's price for this keeper's runs (paid by the vault if a step works).
    // A vault that can't pay it gets no free work: the brain would refuse the step.
    const fee = BigInt(state?.keeper_fee ?? "0");
    if (fee > 0n && todo.length > 0) {
      const free = await chain.freeNear(brain, vault).catch(() => null);
      if (free !== null && !canPayKeeper(fee, free)) {
        log(keeperUnpaid(vault, fee, free));
        continue;
      }
    }
    const byId = new Map(flows);
    for (const d of todo) {
      if (d.remote) {
        const f = byId.get(d.flow)!;
        const n = f.nodes[d.node] as unknown as RemoteNode;
        const r = n.action.remote!;
        const what = `${vault} flow ${d.flow} node ${d.node} "${d.label}" is due on another chain (${remoteNote(r, state!)})`;
        if (!runner) {
          log(`${what}: run with --send --remote to run it`);
          continue;
        }
        const need = runner.needs(r, state!);
        if (need) {
          log(`${what}: ${need}; not stepped`);
          continue;
        }
        log(what);
        await freshen(vault);
        const h = hidden.get(d.flow);
        const stepped = await runner.step(vault, state!, d.flow, d.node, n, f.ledger.nodes[d.node].inbox, h ? { reveal: h.nodes[d.node], salt: h.salts[d.node] } : undefined)
          .catch((e) => (log(`  remote step failed: ${(e as Error).message.slice(0, 200)}`), false));
        if (stepped) {
          ran++;
          break; // one step per vault per round
        }
        continue;
      }
      if (d.price) {
        const s = d.price.source;
        if (boards.includes(s.contract)) await freshen(vault);
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
