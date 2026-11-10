import { describe, expect, it } from "vitest";
import { EMPTY_BOOK, nodeReadout, parseGecko, parseGeckoChart, parseMeteoraPools, parseRefNear, type LiveBook } from "./quotes";

const live: LiveBook = {
  status: "live",
  updatedAt: 1_000,
  near: { usd: 3.62, changePct: 3.2 },
  sol: { usd: 85.5, changePct: -0.8 },
  eth: { usd: 2100, changePct: 0.1 },
  usdc: { usd: 1, changePct: 0 },
  sparks: { near: [3.5, 3.62], sol: [], eth: [] },
};

describe("live quotes", () => {
  it("parses CoinGecko and refuses a broken payload", () => {
    const parsed = parseGecko({ near: { usd: 3.62, usd_24h_change: 1.2 }, solana: { usd: 80 }, ethereum: { usd: 2000 }, "usd-coin": { usd: 1 } });
    expect(parsed?.near?.usd).toBe(3.62);
    expect(parseGecko({ near: { usd: "nope" } })).toBeNull();
    expect(parseRefNear({ "wrap.near": { price: "3.62" } })).toBe(3.62);
    expect(parseRefNear({})).toBeNull();
    expect(parseGeckoChart({ prices: [[1, 3.5], [2, 3.6]] })).toEqual([3.5, 3.6]);
    expect(parseMeteoraPools({ data: [{ address: "abc", name: "SOL-USDC", apy: 12, tvl: 1000, current_price: 85 }] })).toEqual([
      { address: "abc", name: "SOL-USDC", apy: 12, tvl: 1000, price: 85 },
    ]);
  });

  it("labels a demo entry beside a live price, and never keeps a price after failure", () => {
    const shown = nodeReadout({
      demo: true,
      token: "near",
      demoAmount: "420 NEAR",
      demoEntry: "4.40",
      book: live,
      now: 4_000,
    });
    expect(shown.entryTag).toBe("Demo");
    expect(shown.entry).toBe("4.40");
    expect(shown.amountTag).toBe("Demo");
    expect(shown.currentTag).toBe("live");
    expect(shown.current).toBe("3.62");
    expect(shown.ago).toBe("live · updated 3s ago");

    const down = nodeReadout({ demo: false, token: "near", book: EMPTY_BOOK, now: 4_000 });
    expect(down.current).toBe("price unavailable");
    expect(down.currentTag).toBeNull();
    expect(down.entry).toBe("unknown");
    expect(down.amount).toBe("unknown");
    expect(down.spark).toEqual([]);
  });
});
