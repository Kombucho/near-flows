import { demoPnlSource, type DemoRange } from "../demo/fixtures";

export type ChartPoint = { row: number; count: number };

/** Pele source for the demo portfolio. Null when the demo account is off. */
export function portfolioPnlChart(demo: boolean, range: DemoRange): string | null {
  if (!demo) return null;
  return demoPnlSource(range);
}

/**
 * XY source for Pele. Points are evidenced stage counts on imported DEMO| rows.
 * Price, position, and profit are not part of this series.
 */
export function replayStageChart(events: { sourceRow: number; evidenced: readonly string[] }[]): { source: string; points: ChartPoint[] } | null {
  const points = events
    .filter((event) => Number.isInteger(event.sourceRow) && event.sourceRow > 0)
    .slice(0, 32)
    .map((event) => ({ row: event.sourceRow, count: event.evidenced.length }));
  if (points.length === 0) return null;
  const max = Math.max(1, ...points.map((point) => point.count));
  const labels = points.map((point) => String(point.row)).join(", ");
  const values = points.map((point) => String(point.count)).join(", ");
  return {
    points,
    source: [
      "xychart",
      '  title "Replay"',
      `  x-axis "source row" [${labels}]`,
      `  y-axis "stages recorded" 0 --> ${max}`,
      `  bar [${values}]`,
    ].join("\n"),
  };
}
