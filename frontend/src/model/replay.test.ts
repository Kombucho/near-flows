import { describe, expect, it } from "vitest";
import { parseDemoLog, projectLegs, stagesInLine } from "./replay";

describe("demo log stages", () => {
  it("does not promote signed to sent or landed", () => {
    expect(stagesInLine("the vault checks every byte and signs")).toEqual(["signed"]);
    expect(stagesInLine("keeper reports 0.6 SOL of fees: REFUSED (over the node's 0.5 SOL cap)")).toEqual([
      "failed",
      "reported",
    ]);
  });

  it("keeps a landed line from inventing signed, and a missing leg from looking finished", () => {
    const doc = parseDemoLog(
      [
        "DEMO| ━━━ 4. Robinhood Chain: NVDA take-profit ladder into the ETHNVDA basket ━━━",
        "DEMO| buy (Uniswap v4): 0.03 ETH -> 1 NVDA on the fork; reported, a third to each rung",
      ].join("\n"),
      "test fixture",
    );
    expect(doc.hasTimestamps).toBe(false);
    expect(doc.legs).toHaveLength(1);
    expect(doc.warnings.some((warning) => warning.includes("1 leg"))).toBe(true);
    expect(doc.events[0].evidenced).toEqual(["landed", "reported"]);
    expect(doc.events[0].evidenced).not.toContain("signed");
    expect(doc.events[0].evidenced).not.toContain("sent");
  });

  it("keeps distinct lines and does not invent a second stage list for the same node", () => {
    const doc = parseDemoLog(
      [
        "DEMO| ━━━ 2. Buy a pump.fun memecoin on Solana (mainnet fork) ━━━",
        "DEMO| executed on the fork: coins, tx abc",
        "DEMO| executed on the fork: coins, tx abc",
      ].join("\n"),
    );
    expect(doc.legs).toHaveLength(1);
    expect(doc.events).toHaveLength(2);
    const nodes = [
      { id: "pump", chain: "solana" as const, service: "pump" },
      { id: "near", chain: "near" as const, service: "rhea" },
    ];
    const projected = projectLegs(nodes, doc.events);
    expect(projected.map((item) => item.nodeId)).toEqual(["pump"]);
    expect(projected[0].evidenced).toEqual(["landed"]);
    expect(projected[0].sourceLines).toEqual([2, 3]);
  });

  it("leaves earlier stages not recorded when only landed text is present", () => {
    const doc = parseDemoLog("DEMO| ━━━ 1. Buy a memecoin on Rhea (NEAR) ━━━\nDEMO| executed on the fork: something");
    const [event] = doc.events;
    expect(event.evidenced).toEqual(["landed"]);
    expect(event.evidenced.includes("signed")).toBe(false);
    expect(event.evidenced.includes("sent")).toBe(false);
    expect(event.evidenced.includes("reported")).toBe(false);
  });
});
