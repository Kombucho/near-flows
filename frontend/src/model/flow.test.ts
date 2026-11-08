import { describe, expect, it } from "vitest";
import {
  mergeLensEdit,
  remainderBps,
  serializeFlow,
  validateFlow,
  visibleNodes,
  type FlowNode,
} from "./flow";

function node(partial: Partial<FlowNode> & Pick<FlowNode, "id" | "index" | "input" | "output">): FlowNode {
  return {
    title: partial.id,
    venue: "test",
    chain: "near",
    service: "rhea",
    trigger: { when: "now" },
    routes: [],
    params: [],
    position: { x: 0, y: 0 },
    serializable: true,
    ...partial,
  };
}

describe("flow invariants", () => {
  const near = node({ id: "a", index: 0, input: "near", output: "wrap.testnet", chain: "near", service: "wnear", routes: [{ to: 1, bps: 10_000 }] });
  const swap = node({ id: "b", index: 1, input: "wrap.testnet", output: "meme", chain: "near", service: "rhea" });
  const sol = node({ id: "c", index: 2, input: "sol:SOL", output: "sol:coin", chain: "solana", service: "pump" });

  it("rejects a backward route, a token mismatch, a ninth route, and 10001 bps", () => {
    const back = node({ id: "back", index: 1, input: "wrap.testnet", output: "meme", routes: [{ to: 0, bps: 1 }] });
    expect(validateFlow([near, back]).some((issue) => issue.code === "RouteBackwards")).toBe(true);

    const mismatch = node({ id: "m", index: 0, input: "near", output: "wrap.testnet", routes: [{ to: 1, bps: 1 }] });
    const other = node({ id: "o", index: 1, input: "sol:SOL", output: null, chain: "solana" });
    expect(validateFlow([mismatch, other]).some((issue) => issue.code === "RouteTokenMismatch")).toBe(true);

    const many = node({
      id: "many",
      index: 0,
      input: "near",
      output: "wrap.testnet",
      routes: Array.from({ length: 9 }, (_, i) => ({ to: i + 1, bps: 1 })),
    });
    const targets = Array.from({ length: 9 }, (_, i) =>
      node({ id: `t${i}`, index: i + 1, input: "wrap.testnet", output: null }),
    );
    expect(validateFlow([many, ...targets]).some((issue) => issue.code === "TooManyRoutes")).toBe(true);

    const over = node({ id: "over", index: 0, input: "near", output: "wrap.testnet", routes: [{ to: 1, bps: 10_001 }] });
    const dest = node({ id: "dest", index: 1, input: "wrap.testnet", output: null });
    expect(validateFlow([over, dest]).some((issue) => issue.code === "RoutesOverfull")).toBe(true);
  });

  it("keeps the unallocated share as remainder, not as a stored route", () => {
    const partial = node({ id: "p", index: 0, input: "near", output: "wrap.testnet", routes: [{ to: 1, bps: 3_333 }] });
    expect(remainderBps(partial)).toBe(6_667);
    expect(serializeFlow([partial, swap])[0].next).toEqual([{ to: 1, bps: 3_333 }]);
  });

  it("rejects a collector that is not every or manual", () => {
    const collector = node({ id: "col", index: 0, input: "none", output: "meme", trigger: { when: "now" } });
    expect(validateFlow([collector]).some((issue) => issue.code === "BadTrigger")).toBe(true);
  });

  it("round-trips a filtered edit without dropping hidden nodes", () => {
    const full = [near, swap, sol];
    const lens = { chain: "near" as const, service: "all" as const };
    const visible = visibleNodes(full, lens);
    expect(visible.map((item) => item.id)).toEqual(["a", "b"]);
    const edited = visible.map((item) => (item.id === "a" ? { ...item, title: "Wrap" } : item));
    const merged = mergeLensEdit(full, lens, edited);
    expect(merged.find((item) => item.id === "c")).toEqual(sol);
    expect(merged.find((item) => item.id === "a")?.title).toBe("Wrap");
    expect(merged.find((item) => item.id === "a")?.routes).toEqual(near.routes);
    expect(serializeFlow(merged).map((item) => item.index)).toEqual([0, 1, 2]);
  });
});
