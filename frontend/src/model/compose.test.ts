import { describe, expect, it } from "vitest";
import { validateFlow } from "./flow";
import { composePlan, draftFromPrompt, edgeCaption } from "./compose";

describe("NEAR auto-routing", () => {
  it("builds wrap, hop, vault, then Meteora, forward and same-token", () => {
    const nodes = composePlan({ chain: "solana", protocol: "meteora", actionId: "dlmm", amountNear: "12", startIndex: 0 });
    expect(nodes.map((node) => node.title)).toEqual(["Wrap NEAR", "Intents hop", "Solana vault", "Open a Meteora LP"]);
    expect(nodes.filter((node) => node.auto).map((node) => node.title)).toEqual(["Wrap NEAR", "Intents hop", "Solana vault"]);
    expect(nodes[1].serializable).toBe(false);
    expect(nodes[0].routes).toEqual([{ to: 1, bps: 10_000 }]);
    expect(nodes[1].routes).toEqual([{ to: 2, bps: 10_000 }]);
    expect(nodes[2].routes).toEqual([{ to: 3, bps: 10_000 }]);
    const issues = validateFlow(nodes).map((issue) => issue.code);
    expect(issues).not.toContain("RouteBackwards");
    expect(issues).not.toContain("RouteTokenMismatch");
    expect(issues).not.toContain("RoutesOverfull");
    expect(edgeCaption(nodes[0], 10_000)).toBe("12 NEAR");
    expect(edgeCaption(nodes[2], 10_000)).toBe("all SOL");
  });

  it("does not leave NEAR when the protocol is already on NEAR", () => {
    const nodes = composePlan({ chain: "near", protocol: "linear", actionId: "linear", amountNear: "4", startIndex: 3 });
    expect(nodes).toHaveLength(1);
    expect(nodes[0].chain).toBe("near");
    expect(nodes[0].auto).toBeUndefined();
  });

  it("drafts a template from a short prompt and does not invent an unknown one", () => {
    expect(draftFromPrompt("open an LP on meteora")).toEqual({ chain: "solana", protocol: "meteora", actionId: "dlmm" });
    expect(draftFromPrompt("surprise me")).toBeNull();
  });
});
