import { describe, expect, it } from "vitest";
import { DEMO_ACCOUNT } from "../demo/fixtures";
import type { FlowNode } from "../model/flow";
import { EMPTY_BOOK, type LiveBook } from "../live/quotes";
import { positionOwner, presentCard, serialOf, themeClass } from "./cardModel";

const live: LiveBook = {
  status: "live",
  updatedAt: 1_000,
  source: "CoinGecko",
  near: { usd: 3.62, changePct: 3.2 },
  sol: { usd: 85.5, changePct: -0.8 },
  eth: { usd: 2100, changePct: 0.1 },
  usdc: { usd: 1, changePct: 0 },
  sparks: { near: [], sol: [], eth: [] },
};

function node(partial: Partial<FlowNode> & Pick<FlowNode, "service" | "chain">): FlowNode {
  return {
    id: partial.service,
    index: 0,
    title: partial.title ?? partial.service,
    venue: partial.venue ?? partial.service,
    input: "near",
    output: null,
    trigger: { when: "now" },
    routes: [],
    params: [],
    position: { x: 0, y: 0 },
    serializable: true,
    ...partial,
  };
}

describe("protocol cards", () => {
  it("maps each known protocol onto its theme and leaves unknowns neutral", () => {
    expect(themeClass("wnear", "near")).toBe("t-near");
    expect(themeClass("meteora", "solana")).toBe("t-met");
    expect(themeClass("uniswap-v4", "robinhood")).toBe("t-uni");
    expect(themeClass("jupiter", "solana")).toBe("t-jup");
    expect(themeClass("hyperliquid", "solana")).toBe("t-hl");
    expect(themeClass("intents", "near")).toBe("t-intents");
    expect(themeClass("rhea", "near")).toBe("t-rhea");
    expect(themeClass("linear", "near")).toBe("t-linear");
    expect(themeClass("vault", "solana")).toBe("t-sol");
    expect(themeClass("vault", "robinhood")).toBe("t-evm");
    expect(themeClass("pump", "solana")).toBe("t-neutral");
    expect(themeClass("stonk", "robinhood")).toBe("t-neutral");
  });

  it("numbers the set from the node index", () => {
    expect(serialOf(0)).toBe("NF 001");
    expect(serialOf(11)).toBe("NF 012");
  });

  it("keeps unknown as a word when demo is off, including a zero fixture", () => {
    const face = presentCard({
      node: node({ service: "jupiter", chain: "solana", title: "Jupiter swap", input: "sol:SOL" }),
      demo: false,
      book: EMPTY_BOOK,
      now: 0,
    });
    expect(face.stamp).toBeNull();
    expect(face.value).toBe("unknown");
    expect(face.entry).toBe("unknown");
    expect(face.amount).toBe("unknown");
    expect(face.change).toBe("unknown");
    expect(face.reporter).toBe("no account linked");
    expect(face.stages).toEqual([]);
    expect(face.demo).toBe(false);
  });

  it("stamps a demo Meteora card from the fixture and only lists evidenced stages", () => {
    const face = presentCard({
      node: node({
        service: "meteora",
        chain: "solana",
        title: "Meteora DLMM",
        venue: "Meteora DLMM",
        index: 6,
        input: "sol:SOL",
        output: null,
      }),
      demo: true,
      book: EMPTY_BOOK,
      now: 0,
      evidenced: ["signed", "landed"],
    });
    expect(face.stamp).toBe("DEMO");
    expect(face.value).toBe("$2,100");
    expect(face.damage).toBe("18.6%");
    expect(face.damageLabel).toBe("DEMO · APR");
    expect(face.source).toBe("labelled demo");
    expect(face.reporter).toBe(DEMO_ACCOUNT.keeper);
    expect(face.stages.map((stage) => stage.name)).toEqual(["signed", "landed"]);
    expect(face.serial).toBe("NF 007");
    expect(face.change).toBe("−0.4%");
  });

  it("labels a live wrap quote and does not invent a position value", () => {
    const face = presentCard({
      node: node({ service: "wnear", chain: "near", title: "Wrap NEAR", venue: "wNEAR", input: "near", output: "wrap.testnet" }),
      demo: false,
      book: live,
      now: 12_000,
    });
    expect(face.value).toBe("unknown");
    expect(face.damage).toBe("$3.62");
    expect(face.damageLabel).toBe("LIVE · wNEAR");
    expect(face.source).toBe("quote: CoinGecko · 11s ago");
    expect(face.stamp).toBeNull();
    expect(face.attribution).toBeNull();
  });

  it("dims a quote older than a minute and attributes Uniswap without its logo", () => {
    const face = presentCard({
      node: node({
        service: "uniswap-v4",
        chain: "robinhood",
        title: "Buy NVDA",
        venue: "Uniswap v4",
        input: "evm:4663:eth",
        output: "evm:4663:nvda",
      }),
      demo: false,
      book: { ...live, updatedAt: 0 },
      now: 120_000,
    });
    expect(face.stale).toBe(true);
    expect(face.source).toContain("stale");
    expect(face.attribution).toBe("Uniswap is a trademark of Uniswap Labs.");
    expect(face.themeId).toBe("uni");
  });

  it("gives a protocol's sample position to one node", () => {
    const lp = node({ id: "lp", service: "meteora", chain: "solana", title: "Provide liquidity", index: 1 });
    const sweep = node({ id: "sweep", service: "meteora", chain: "solana", title: "Sweep fees", index: 2, input: "none" });
    const owners = positionOwner([lp, sweep]);
    expect(owners.has("lp")).toBe(true);
    expect(owners.has("sweep")).toBe(false);
    const other = presentCard({ node: sweep, demo: true, book: EMPTY_BOOK, now: 0, usePosition: false });
    expect(other.value).toBe("unknown");
    expect(other.damage).toBe("unknown");
    expect(other.verb).toBe("Sweep");
  });
});
