import { describe, expect, it } from "vitest";
import { parseDemoLog, projectLegs, stagesInLine } from "./replay";

/** Sanitized copies of say! format strings in contracts/sandbox/tests/demo.rs. Amounts are placeholders, not a run. */
const PRODUCER = [
  "DEMO| prices: NEAR $2.00 (Pyth), MEME $1 (price board, 2 of 3 attesters signed)",
  "DEMO| ━━━ 2. Buy a pump.fun memecoin on Solana (mainnet fork) ━━━",
  "DEMO| the fork gives the vault's Solana address 2 SOL (a cheat code: no real money)",
  "DEMO| keeper asks to sign a 0.126 SOL buy: REFUSED (over the template's cap), nothing signed",
  "DEMO| keeper builds a 0.05 SOL buy with pump.fun's SDK; the vault checks every byte and signs",
  "DEMO| executed on the fork: 1 coins (6 decimals) in the vault's own token account, tx abcd…",
  "DEMO| keeper steps the sweep: claim_fee2 + close wSOL, checked and signed by the vault, executed: +1 SOL",
  "DEMO| keeper reports 0.6 SOL of fees: REFUSED (over the node's 0.5 SOL cap)",
  "DEMO| keeper steps the buy: 1 SOL of fees -> 2 coins on the fork (signed minimum 2), reported",
  "DEMO| buy (Uniswap v4): 0.03 ETH -> 1 NVDA on the fork; reported, a third to each rung",
].join("\n");

describe("demo producer markers", () => {
  it("maps only the phrases the producer emits", () => {
    expect(stagesInLine("prices: NEAR $2.00 (Pyth), MEME $1 (price board, 2 of 3 attesters signed)")).toEqual([]);
    expect(stagesInLine("keeper asks to sign a 0.126 SOL buy: REFUSED (over the template's cap), nothing signed")).toEqual(["failed"]);
    expect(stagesInLine("the vault checks every byte and signs")).toEqual(["signed"]);
    expect(stagesInLine("keeper asks the vault to sign the buy directly, outside any flow: REFUSED (owner only), nothing signed")).toEqual(["failed"]);
    expect(stagesInLine("executed on the fork: 1 coins (6 decimals) in the vault's own token account, tx abcd…; reported")).toEqual(["landed", "reported"]);
    expect(stagesInLine("checked and signed by the vault, executed: +1 SOL")).toEqual(["signed", "landed"]);
    expect(stagesInLine("keeper reports 0.6 SOL of fees: REFUSED (over the node's 0.5 SOL cap)")).toEqual(["reported", "failed"]);
    expect(stagesInLine("1 SOL of fees -> 2 coins on the fork (signed minimum 2), reported")).toEqual(["reported"]);
    expect(stagesInLine("buy (Uniswap v4): 0.03 ETH -> 1 NVDA on the fork; reported, a third to each rung")).toEqual(["reported"]);
    expect(stagesInLine("the vault holds 500 USDC on the fork")).toEqual([]);
  });

  it("keeps source row order and leaves absent stages not recorded", () => {
    const doc = parseDemoLog(PRODUCER, "sanitized producer fixture");
    expect(doc.hasTimestamps).toBe(false);
    expect(doc.timing).toMatch(/display pacing/);
    expect(doc.events.map((event) => event.sourceRow)).toEqual([1, 3, 4, 5, 6, 7, 8, 9, 10]);
    const signed = doc.events.find((event) => event.raw.includes("every byte"));
    expect(signed?.evidenced).toEqual(["signed"]);
    expect(signed?.notRecorded).toEqual(["sent", "landed", "reported", "failed"]);
    const landed = doc.events.find((event) => event.raw.startsWith("executed on the fork"));
    expect(landed?.evidenced).toEqual(["landed"]);
    expect(landed?.notRecorded).toContain("signed");
    expect(landed?.notRecorded).toContain("sent");
    expect(doc.events.every((event) => !event.evidenced.includes("sent"))).toBe(true);
  });

  it("keeps a truncated file incomplete and a duplicate row in order", () => {
    const truncated = parseDemoLog("DEMO| ━━━ 1. Buy a memecoin on Rhea (NEAR), guarded by a market floor ━━━\nDEMO| keeper steps node 1: REFUSED. The vault's floor");
    expect(truncated.warnings.some((warning) => warning.includes("Done"))).toBe(true);
    expect(truncated.events).toHaveLength(1);
    expect(truncated.events[0].evidenced).toEqual(["failed"]);
    expect(truncated.events[0].notRecorded).toContain("landed");
    expect(new Set(truncated.legs.map((leg) => leg.n)).size).toBe(1);

    const duplicated = parseDemoLog(
      [
        "DEMO| ━━━ 2. Buy a pump.fun memecoin on Solana (mainnet fork) ━━━",
        "DEMO| executed on the fork: 1 coins (6 decimals) in the vault's own token account, tx abcd…",
        "DEMO| executed on the fork: 1 coins (6 decimals) in the vault's own token account, tx abcd…",
        "DEMO| ━━━ 2. Buy a pump.fun memecoin on Solana (mainnet fork) ━━━",
      ].join("\n"),
    );
    expect(duplicated.events).toHaveLength(2);
    expect(duplicated.events.map((event) => event.sourceRow)).toEqual([2, 3]);
    expect(duplicated.warnings.some((warning) => warning.includes("Leg 2"))).toBe(true);
    const projected = projectLegs(
      [
        { id: "pump", chain: "solana", service: "pump" },
        { id: "near", chain: "near", service: "rhea" },
      ],
      duplicated.events,
    );
    expect(projected).toHaveLength(1);
    expect(projected[0].nodeId).toBe("pump");
    expect(projected[0].evidenced).toEqual(["landed"]);
    expect(projected[0].notRecorded).toEqual(["signed", "sent", "reported", "failed"]);
    expect(projected[0].sourceRows).toEqual([2, 3]);
  });

  it("projects nothing when the file has no DEMO lines", () => {
    const doc = parseDemoLog("ordinary text\n", "empty");
    expect(doc.events).toEqual([]);
    expect(doc.warnings[0]).toMatch(/no DEMO/);
  });
});
