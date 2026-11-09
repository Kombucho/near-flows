import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { due, type Flow, type FlowNode, type NodeState } from "./flows.ts";
import {
  amountOf, drop, evmArgs, evmPlan, evmValues, lessSlippage, loadState, nextFor, orphans, parseToken, reportable, saveState, solPlan,
  statePath, stepResult, upsert, v4QuoteCalldata, type EvmConfig, type InFlight, type Pending, type SolConfig,
} from "./remote.ts";

const lib = (f: string) => JSON.parse(readFileSync(new URL(`../../../nodes/${f}`, import.meta.url), "utf8"));
const evmLib = lib("evm-templates.json");
const solLib = lib("sol-templates.json");
const fill = (t: unknown, kv: Record<string, string>) => JSON.parse(Object.entries(kv).reduce((s, [k, v]) => s.replaceAll(`{${k}}`, v), JSON.stringify(t)));

const robinhood = (over: Partial<EvmConfig> = {}): EvmConfig => ({
  signer: "v1.signer", chain_id: 4663, address: "0x00000000000000000000000000000000000000aa", max_fee: "1000000000", max_tip: "100000000", gas_cap: 1_500_000, next_nonce: 7,
  templates: [...evmLib.chains.robinhood.templates, fill(evmLib.chains.robinhood.intents_deposits[0], { deposit_address: "0x00000000000000000000000000000000d3905170", max_wei: "100000000000000000" })],
  ...over,
});
const MINT = "GDFVPqix6jYh4xvr1CBx14JWUaLMTvAT2rYA9FyApump";
const solCfg = (): SolConfig => ({
  signer: "v1.signer", address: "CBbbB6PNqsW1jNzHebzu3YrKDXQ5UQo9eGbqxvCWvjhG", max_cu_price: 1_000_000, max_cu_limit: 1_400_000,
  templates: [
    fill(solLib.templates.find((t: any) => t.name === "pump_buy"), { mint: MINT, max_lamports: "100000000", floor_per_lamport: "30" }),
    fill(solLib.templates.find((t: any) => t.name === "pump_sell"), { mint: MINT, max_units: "1000000000000", floor_num: "1", floor_den: "40" }),
    fill(solLib.templates.find((t: any) => t.name === "intents_deposit_sol"), { deposit_address: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", max_lamports: "300000000" }),
    solLib.templates.find((t: any) => t.name === "wsol_wrap"),
  ],
});

// ---------- due detection ----------

const st = (inbox: string, extra: Partial<NodeState> = {}): NodeState => ({ inbox, running: "0", runs: 0, last_run_ns: 0, failures: 0, ...extra });
const rnode = (input: string, call: object, when: FlowNode["when"] = { when: "now" }): FlowNode => ({ input, when, action: { label: "r", price: null, remote: { call, out: "sol:SOL" } } });

test("remote and hop nodes are due with their chain; a flow waiting on a remote report is not", () => {
  const f: Flow = { name: "f", ledger: { nodes: [st("100"), st("5"), st("0"), st("9")], busy: null }, nodes: [
    rnode("intents:sol.omft.near", { chain: "hop" }),
    rnode("sol:SOL", { chain: "sol", template: "pump_buy", amount_field: 1, min_field: 0 }),
    rnode("sol:SOL", { chain: "sol", template: "pump_buy", amount_field: 1 }), // empty: nothing to spend
    rnode("evm:4663:eth", { chain: "evm", template: "v4_swap_eth_nvda", amount_leaf: 8, min_leaf: 9, deadline_leaf: 15 }),
  ] };
  assert.deepEqual(due([[3, f]], 0n).map((d) => [d.node, d.remote, d.chain]), [[0, true, "hop"], [1, true, "sol"], [3, true, "evm"]]);
  const waiting = { ...f, ledger: { ...f.ledger, busy: 0 } }; // the hop is pending: the brain keeps the flow busy until its report
  assert.equal(due([[3, waiting]], 0n).length, 0);
});

test("what the brain will spend: the inbox, a DCA chunk of it, nothing for a collector", () => {
  assert.equal(amountOf("sol:SOL", { when: "now" }, "500"), 500n);
  assert.equal(amountOf("sol:SOL", { when: "every", chunk: "200" }, "500"), 200n);
  assert.equal(amountOf("sol:SOL", { when: "every", chunk: "900" }, "500"), 500n);
  assert.equal(amountOf("none", { when: "every", chunk: "1" }, "0"), 0n);
});

test("remote tokens: native, ERC-20, SOL, a mint; anything else is not measurable", () => {
  assert.deepEqual(parseToken("evm:4663:eth"), { chain: "evm", chainId: 4663, token: "eth" });
  assert.deepEqual(parseToken("evm:4663:0xD0601ce157db5bdc3162bbac2a2c8af5320d9eec"), { chain: "evm", chainId: 4663, token: "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec" });
  assert.deepEqual(parseToken("sol:SOL"), { chain: "sol", mint: "SOL" });
  assert.deepEqual(parseToken(`sol:${MINT}`), { chain: "sol", mint: MINT });
  for (const t of ["evm:base:eth", "evm:1:nvda", "intents:sol.omft.near", "sol:", "near"]) assert.equal(parseToken(t), null, t);
});

// ---------- EVM builders ----------

test("a Uniswap v4 swap: the keeper quotes the pool and fills only the minimum; the brain fills amount, deadline, nonce, fee cap", () => {
  const cfg = robinhood();
  const p = evmPlan(cfg, { chain: "evm", template: "v4_swap_eth_nvda", amount_leaf: 8, min_leaf: 9, deadline_leaf: 15 });
  assert.ok(!("skip" in p), JSON.stringify(p));
  assert.equal(p.leaves, 17, "16 leaves + the ETH value");
  assert.equal(p.spendsEth, true);
  assert.deepEqual(p.quote, { currency0: "0x0000000000000000000000000000000000000000", currency1: "0xd0601ce157db5bdc3162bbac2a2c8af5320d9eec", fee: 10000, tickSpacing: 200, hooks: "0x0000000000000000000000000000000000000000", zeroForOne: true });
  const min = lessSlippage(1_000_000n, 100);
  assert.equal(min, 990_000n);
  const v = evmValues(p, min);
  assert.equal(v.length, 17);
  assert.deepEqual(v.filter((x, i) => i !== 9), new Array(16).fill("0"), "everything else is the brain's");
  assert.equal(v[9], "990000");
  // The nonce is the brain's next one, the fee cap the owner's; the tip the network's, capped.
  assert.deepEqual(evmArgs(v, cfg, 5_000_000_000n, p.gas), { chain: "evm", values: v, nonce: 7, max_fee: "1000000000", max_tip: "100000000", gas: 1_000_000 });
  assert.equal(evmArgs(v, cfg, 1n, p.gas).max_tip, "1");
  const back = evmPlan(cfg, { chain: "evm", template: "v4_swap_nvda_eth", amount_leaf: 8, min_leaf: 9, deadline_leaf: 15 });
  assert.ok(!("skip" in back) && back.quote?.zeroForOne === false && back.leaves === 16 && !back.spendsEth);
});

test("the basket zap and the Intents deposit: nothing to quote, the brain's floor is the minimum", () => {
  const cfg = robinhood();
  const zap = evmPlan(cfg, { chain: "evm", template: "pairtoken_ethnvda_zap_eth", amount_leaf: 1, min_leaf: 2 });
  assert.ok(!("skip" in zap) && zap.quote === undefined);
  assert.deepEqual(evmValues(zap, 0n), ["0", "0", "0", "0", "0"]);
  const dep = evmPlan(cfg, { chain: "evm", template: "intents_deposit_eth", amount_leaf: 0 });
  assert.ok(!("skip" in dep) && dep.gas === 100_000);
  assert.deepEqual(evmValues(dep, 0n), ["0"]);
});

test("no builder, a number the keeper would choose, or no address: skipped with the reason, never guessed", () => {
  const cfg = robinhood();
  assert.deepEqual(evmPlan(cfg, { chain: "evm", template: "smartlp_amzn_withdraw_usdg", amount_leaf: 0 }), { skip: "no builder for smartlp_amzn_withdraw_usdg: run it by hand" });
  assert.deepEqual(evmPlan(cfg, { chain: "evm", template: "pairtoken_ethnvda_zap_eth", min_leaf: 2 }), { skip: "pairtoken_ethnvda_zap_eth: leaf 1 is a number the keeper would have to choose: run it by hand" });
  assert.deepEqual(evmPlan(cfg, { chain: "evm", template: "v4_swap_eth_nvda", amount_leaf: 8, deadline_leaf: 15 }), { skip: "v4_swap_eth_nvda: not a Uniswap v4 single swap the keeper can quote" });
  assert.match((evmPlan(robinhood({ address: null }), { chain: "evm", template: "intents_deposit_eth", amount_leaf: 0 }) as any).skip, /no EVM address/);
  assert.match((evmPlan(robinhood({ templates: [] }), { chain: "evm", template: "intents_deposit_eth", amount_leaf: 0 }) as any).skip, /no template/);
  assert.equal((evmPlan(robinhood({ gas_cap: 300_000 }), { chain: "evm", template: "pairtoken_ethnvda_zap_eth", amount_leaf: 1, min_leaf: 2 }) as any).gas, 300_000, "never over the owner's gas cap");
});

test("the v4 quoter's calldata, byte for byte as cast encodes it", () => {
  const p = evmPlan(robinhood(), { chain: "evm", template: "v4_swap_eth_nvda", amount_leaf: 8, min_leaf: 9, deadline_leaf: 15 }) as any;
  assert.equal(v4QuoteCalldata(p.quote, 30_000_000_000_000_000n),
    "0xaa9d21cb0000000000000000000000000000000000000000000000000000000000000020" + "0".repeat(64) + "000000000000000000000000d0601ce157db5bdc3162bbac2a2c8af5320d9eec"
    + "0000000000000000000000000000000000000000000000000000000000002710" + "00000000000000000000000000000000000000000000000000000000000000c8" + "0".repeat(64)
    + "0000000000000000000000000000000000000000000000000000000000000001" + "000000000000000000000000000000000000000000000000006a94d74f430000"
    + "0000000000000000000000000000000000000000000000000000000000000100" + "0".repeat(64));
});

// ---------- Solana builders ----------

test("pump.fun buy and sell, the Intents deposit: the pinned mint or address, the node's amount, the owner's floor", () => {
  const cfg = solCfg();
  assert.deepEqual(solPlan(cfg, { chain: "sol", template: "pump_buy", amount_field: 1, min_field: 0 }, 50_000_000n), { kind: "pump_buy", mint: MINT, lamports: 50_000_000n, floor: 1_500_000_000n });
  assert.deepEqual(solPlan(cfg, { chain: "sol", template: "pump_sell", amount_field: 0, min_field: 1 }, 4_000n), { kind: "pump_sell", mint: MINT, units: 4_000n, floor: 100n });
  assert.deepEqual(solPlan(cfg, { chain: "sol", template: "intents_deposit_sol", amount_field: 0 }, 99_986_000n), { kind: "pay_sol", to: "9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin", lamports: 99_986_000n });
  assert.deepEqual(solPlan(cfg, { chain: "sol", template: "wsol_wrap", amount_field: 0 }, 1n), { skip: "no builder for wsol_wrap: run it by hand" });
  assert.match((solPlan(cfg, { chain: "sol", template: "pump_buy", amount_field: 0 }, 1n) as any).skip, /amount_field 0 isn't 1/);
  assert.match((solPlan(cfg, { chain: "sol", template: "intents_deposit_usdc", amount_field: 0 }, 1n) as any).skip, /no template/);
  assert.match((solPlan({ ...cfg, address: null }, { chain: "sol", template: "pump_buy", amount_field: 1 }, 1n) as any).skip, /no Solana address/);
});

// ---------- reports ----------

test("what may be reported: at least the signed minimum, at most the cap", () => {
  assert.equal(reportable(150n, 100n, 200n), 150n);
  assert.equal(reportable(250n, 100n, 200n), 200n, "more than the cap: the cap");
  assert.equal(reportable(99n, 100n, 200n), null, "under the minimum: it didn't land (or not all of it); the keeper never says failed");
  assert.equal(reportable(0n, 0n, 0n), 0n, "a deposit: nothing comes out");
  assert.equal(reportable(-5n, 0n, 0n), null, "a hop not landed yet");
});

test("the step's events: the brain's bounds and what it signed", () => {
  const ev = (event: string, data: object) => `EVENT_JSON:${JSON.stringify({ standard: "flows", version: "1.0.0", event, data: [{ vault: "v", ...data }] })}`;
  const logs = ["noise", ev("remote_step", { flow: 2, node: 1, amount: "5", min_out: "4", max_out: "9", hash: "0xab", expires_ns: null }), ev("evm_signed", { hash: "0xab", raw: "0x02f8", r: "", s: "", v: 0 })];
  assert.deepEqual(stepResult(logs, 2, 1), { step: { min: "4", max: "9", hash: "0xab", amount: "5" }, evmRaw: "0x02f8", solSignature: null, hopSent: null, failed: null });
  const unsigned = [ev("remote_step", { flow: 2, node: 1, amount: "5", min_out: "0", max_out: "0", hash: "h", expires_ns: 1 }), ev("step_failed", { flow: 2, node: 1, why: "not signed" })];
  assert.equal(stepResult(unsigned, 2, 1).failed?.why, "not signed");
  const hop = [ev("remote_step", { flow: 0, node: 0, amount: "100", min_out: "90", max_out: "100", hash: "WITHDRAW_TO:x", expires_ns: null }), ev("hop_sent", { flow: 0, node: 0, amount: "100", sent: "100" })];
  assert.equal(stepResult(hop, 0, 0).hopSent?.sent, "100");
});

// ---------- resume ----------

const pend = (flow: number, node: number): Pending => ({ flow, node, amount: "1", min_out: "0", max_out: "1", expires_ns: null, signed: true, hash: "h" });
const fl = (status: InFlight["status"], kind: InFlight["kind"] = "evm", extra: Partial<InFlight> = {}): InFlight => ({ vault: "v.near", flow: 1, node: 2, kind, out: null, address: "a", status, startedMs: 0, ...extra });

test("after a restart: broadcast what was signed, keep waiting on what was sent, never step again", () => {
  assert.equal(nextFor(fl("signed"), pend(1, 2)), "broadcast", "signed, not sent: send the same bytes");
  assert.equal(nextFor(fl("sent"), pend(1, 2)), "wait");
  assert.equal(nextFor(fl("sent", "hop", { before: "0" }), pend(1, 2)), "wait");
  assert.equal(nextFor(fl("stepping", "hop", { before: "5" }), pend(1, 2)), "wait", "a hop went out by NEAR: only watch it land");
  assert.equal(nextFor(fl("stepping", "evm"), pend(1, 2)), "settle", "signed bytes never seen: the owner's");
  assert.equal(nextFor(fl("settle"), pend(1, 2)), "settle");
  for (const s of ["stepping", "signed", "sent", "settle"] as const) assert.equal(nextFor(fl(s), null), "drop", `${s}: nothing pending on the brain any more`);
});

test("the state file: outside the repo, private, survives a restart; unknown pending steps are the owner's", () => {
  const dir = mkdtempSync(join(tmpdir(), "flows-keeper-"));
  assert.match(statePath("k.near"), /\.flows[/\\]keeper[/\\]k\.near\.json$/);
  const path = statePath("k/../x.near", dir);
  assert.equal(path, join(dir, "k_.._x.near.json"), "an account id can't walk out of the folder");
  assert.deepEqual(loadState(path), { inflight: [] });
  let s = upsert({ inflight: [] }, fl("signed", "evm", { raw: "0x02", txHash: "0xh" }));
  s = upsert(s, fl("sent", "hop", { flow: 3, node: 0, before: "10" }));
  s = upsert(s, fl("sent", "evm", { raw: "0x02", txHash: "0xh" })); // same step: replaced, not doubled
  saveState(path, s);
  assert.equal(statSync(path).mode & 0o777, 0o600);
  const back = loadState(path);
  assert.equal(back.inflight.length, 2);
  assert.equal(back.inflight.find((f) => f.flow === 1)?.status, "sent");
  assert.deepEqual(orphans([pend(1, 2), pend(3, 0), pend(4, 4)], "v.near", back).map((p) => p.flow), [4]);
  assert.equal(drop(back, { vault: "v.near", flow: 1, node: 2 }).inflight.length, 1);
});
