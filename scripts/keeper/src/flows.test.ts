import { test } from "node:test";
import assert from "node:assert/strict";
import { due, fired, priceOf, runnable, type Flow, type FlowNode, type NodeState, type VaultState } from "./flows.ts";

const MIN = 60_000_000_000;
const node = (when: FlowNode["when"], input = "wrap.testnet", price: FlowNode["action"]["price"] = null): FlowNode => ({ input, when, action: { label: "x", price } });
const st = (inbox: string, extra: Partial<NodeState> = {}): NodeState => ({ inbox, running: "0", runs: 0, last_run_ns: 0, failures: 0, ...extra });
const flow = (nodes: FlowNode[], states: NodeState[], busy: number | null = null): Flow => ({ name: "f", nodes, ledger: { nodes: states, busy } });

test("now-nodes with money are due; empty, paused and manual ones are not", () => {
  const f = flow([node({ when: "now" }), node({ when: "now" }), node({ when: "now" }), node({ when: "manual" })],
    [st("10"), st("0"), st("10", { failures: 3 }), st("10")]);
  assert.deepEqual(due([[0, f]], 0n).map((d) => d.node), [0]);
});

test("a flow with a step in flight waits", () => {
  const f = flow([node({ when: "now" })], [st("10")], 0);
  assert.equal(due([[0, f]], 0n).length, 0);
});

test("schedules: after a time, and every N (DCA)", () => {
  const f = flow([node({ when: "after", at_ns: 100 }), node({ when: "every", every_ns: MIN, chunk: "1" })],
    [st("5"), st("5", { runs: 1, last_run_ns: 1_000 })]);
  assert.deepEqual(due([[0, f]], 99n).map((d) => d.node), []);
  assert.deepEqual(due([[0, f]], 100n).map((d) => d.node), [0]);
  assert.deepEqual(due([[0, f]], BigInt(1_000 + MIN)).map((d) => d.node), [0, 1]);
});

test("collectors run on their schedule even with no money and even after failures", () => {
  const c = node({ when: "every", every_ns: MIN, chunk: "1" }, "none");
  const f = flow([c], [st("0", { failures: 7, last_run_ns: 5 * MIN })]);
  assert.equal(due([[0, f]], BigInt(5 * MIN + 1)).length, 0);
  assert.equal(due([[0, f]], BigInt(6 * MIN)).length, 1);
  const fresh = flow([c], [st("0")]);
  assert.equal(due([[0, fresh]], 0n).length, 1, "never ran: due now");
});

test("price triggers: checked with a free view before sending anything", () => {
  const src = { contract: "ref.testnet", method: "get_return", args: "{}", per: (10n ** 24n).toString() };
  const f = flow([node({ when: "price_above", price: (3n * 10n ** 24n).toString() }, "wrap.testnet", src)], [st("5")]);
  const [d] = due([[0, f]], 0n);
  assert.ok(d.price);
  assert.equal(fired(d, priceOf((2n * 10n ** 24n).toString(), src.per)), false);
  assert.equal(fired(d, priceOf((3n * 10n ** 24n).toString(), src.per)), true);
  const g = flow([node({ when: "price_below", price: "100" })], [st("5")]);
  assert.equal(due([[0, g]], 0n).length, 0, "no price source: the vault would refuse, so don't try");
});

test("a relay vault's flows come from its brain record: ids are positions, deleted flows skipped", () => {
  const f = flow([node({ when: "now" })], [st("10")]);
  const v: VaultState = { owner: "alice.near", keeper: "k.near", busy: null, flows: [f, null, f] };
  assert.deepEqual(runnable(v, "k.near").map(([id]) => id), [0, 2]);
});

test("a vault with another keeper, a step in flight or no record gives the keeper nothing", () => {
  const f = flow([node({ when: "now" })], [st("10")]);
  assert.equal(runnable({ owner: "a", keeper: "other.near", busy: null, flows: [f] }, "k.near").length, 0);
  assert.equal(runnable({ owner: "a", keeper: "k.near", busy: { id: 3 }, flows: [f] }, "k.near").length, 0);
  assert.equal(runnable(null, "k.near").length, 0);
});

test("remote nodes come back marked, oracle triggers included, for the chain's runner", () => {
  const remote = (when: FlowNode["when"]): FlowNode => ({ input: "evm:4663:eth", when, action: { label: "r", price: null, remote: { out: "evm:4663:nvda" } } });
  const f = flow([remote({ when: "now" }), remote({ when: "price_above", price: "1" })], [st("5"), st("5")]);
  assert.deepEqual(due([[0, f]], 0n).map((d) => [d.node, d.remote, d.price]), [[0, true, undefined], [1, true, undefined]]);
});
