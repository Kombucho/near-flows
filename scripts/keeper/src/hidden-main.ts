// Hidden flows, as the owner: save one, show it, or hide it again.
//
//   npm run hidden -- save   <vault> <nodes.json> [--name label]
//   npm run hidden -- reveal <vault> <flow_id>
//   npm run hidden -- hide   <vault> <flow_id>
//
// The plan (nodes + one fresh salt per node) is kept in ~/.flows/hidden/<vault>.json;
// only fingerprints go on chain. Share that file with your keeper so it can run
// the steps; nobody else can read the plan. `reveal` makes the flow public for
// good (history can't be unpublished); `hide` hides what comes next.
//
// <vault> is a relay vault: requests go through its `x` door, the flow is read from its brain.
// Environment: OWNER_ACCOUNT_ID, NEAR_NETWORK (default testnet), NEAR_RPC.

import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { Chain, loadKey } from "./chain.ts";
import type { Flow } from "./flows.ts";
import { fingerprint, loadStore, newSalt, saveStore, type Node } from "./hidden.ts";

const { values: opt, positionals: [cmd, vault, arg] } = parseArgs({ allowPositionals: true, options: { name: { type: "string", default: "" } } });
const network = process.env.NEAR_NETWORK ?? "testnet";
const rpc = process.env.NEAR_RPC ?? "https://test.rpc.fastnear.com";
const owner = process.env.OWNER_ACCOUNT_ID ?? "";
if (!owner || !vault || !arg) throw new Error("usage: hidden save|reveal|hide <vault> <nodes.json|flow_id>; set OWNER_ACCOUNT_ID");
const chain = new Chain(rpc, vault, owner, loadKey(network, owner));
const store = loadStore(vault);

if (cmd === "save") {
  const nodes: Node[] = JSON.parse(readFileSync(arg, "utf8"));
  const salts = nodes.map(newSalt);
  const fingerprints = nodes.map((n, i) => fingerprint(n, salts[i]));
  const id = await chain.vaultCall(vault, "save_hidden_flow", { name: opt.name, inputs: nodes.map((n) => n.input), fingerprints });
  store[String(id)] = { nodes, salts };
  saveStore(vault, store);
  console.log(`hidden flow ${id} saved: ${nodes.length} node(s), plan in ~/.flows/hidden/${vault}.json`);
} else if (cmd === "reveal") {
  const plan = store[arg];
  if (!plan) throw new Error(`no plan for flow ${arg} in ~/.flows/hidden/${vault}.json`);
  await chain.vaultCall(vault, "reveal_flow", { flow_id: Number(arg), nodes: plan.nodes, salts: plan.salts });
  delete store[arg];
  saveStore(vault, store);
  console.log(`flow ${arg} is public now`);
} else if (cmd === "hide") {
  const f = await chain.brainView<Flow>(await chain.brainOf(vault), vault, "get_flow", { flow_id: Number(arg) });
  const nodes = f.nodes as unknown as Node[];
  const salts = nodes.map(newSalt); // fresh salts: the old nodes are public, the new commitment isn't linkable
  await chain.vaultCall(vault, "hide_flow", { flow_id: Number(arg), fingerprints: nodes.map((n, i) => fingerprint(n, salts[i])) });
  store[arg] = { nodes, salts };
  saveStore(vault, store);
  console.log(`flow ${arg} is hidden from now on (its past stays in the chain's history)`);
} else throw new Error(`unknown command ${cmd}`);
