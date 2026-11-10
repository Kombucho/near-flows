import { describe, expect, it } from "vitest";
import { PALETTE } from "./draft";
import { CHAIN_PINS, paletteGroups } from "./shell";

describe("canvas pins", () => {
  it("offers NEAR, Solana, and EVM, and does not invent Hyperliquid", () => {
    expect(CHAIN_PINS.map((pin) => pin.label)).toEqual(["NEAR", "Solana", "EVM"]);
    expect(JSON.stringify(CHAIN_PINS).toLowerCase()).not.toContain("hyperliquid");
    expect(PALETTE.some((item) => item.service === "jupiter" || item.venue.toLowerCase().includes("hyper"))).toBe(false);
  });

  it("keeps Jupiter as a Solana protocol without a flow-node recipe", () => {
    const solana = CHAIN_PINS.find((pin) => pin.id === "solana");
    expect(solana?.protocols.map((protocol) => protocol.label)).toEqual(["Meteora", "pump.fun", "Jupiter"]);
    expect(solana?.protocols.find((protocol) => protocol.id === "jupiter")?.flowNode).toBe(false);
    const groups = paletteGroups("solana", "jupiter");
    expect(groups).toHaveLength(1);
    expect(groups[0].items).toHaveLength(0);
  });

  it("groups addable nodes by the selected chain without dropping other chains from the catalog", () => {
    const near = paletteGroups("near", "all").flatMap((group) => group.items.map((item) => item.id));
    expect(near).toContain("node.wrap_near");
    expect(near).toContain("intents-hop");
    expect(near).not.toContain("sol_template.pump_buy");
    expect(PALETTE.some((item) => item.chain === "solana" && item.service === "meteora")).toBe(true);
    expect(paletteGroups("robinhood", "uniswap-v4").flatMap((group) => group.items.map((item) => item.service))).toEqual(["uniswap-v4"]);
  });
});
