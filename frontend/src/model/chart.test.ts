import { describe, expect, it } from "vitest";
import { portfolioPnlChart, replayStageChart } from "./chart";

describe("replay chart", () => {
  it("draws one bar per imported row and invents no price series", () => {
    const chart = replayStageChart([
      { sourceRow: 4, evidenced: ["signed"] },
      { sourceRow: 9, evidenced: ["signed", "landed"] },
      { sourceRow: 12, evidenced: [] },
    ]);
    expect(chart?.points).toEqual([
      { row: 4, count: 1 },
      { row: 9, count: 2 },
      { row: 12, count: 0 },
    ]);
    expect(chart?.source).toContain('title "Replay"');
    expect(chart?.source.toLowerCase()).not.toMatch(/price|profit|pnl|\$/);
  });

  it("returns nothing when there is no imported row", () => {
    expect(replayStageChart([])).toBeNull();
  });

  it("builds a labelled demo PnL chart only while demo is on", () => {
    expect(portfolioPnlChart(false, "1W")).toBeNull();
    expect(portfolioPnlChart(true, "1W")).toContain('title "Demo PnL 1W"');
  });
});