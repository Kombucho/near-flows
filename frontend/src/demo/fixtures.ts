/**
 * Labelled demo account. These numbers are a sample portfolio for the terminal.
 * They are not balances, not a flow, and not a replay log.
 */
export const DEMO_KIND = "labelled-demo-account" as const;

export type DemoRange = "1D" | "1W" | "1M" | "All";

export type DemoHolding = {
  id: string;
  symbol: string;
  name: string;
  chain: "NEAR" | "Solana" | "EVM";
  amount: string;
  valueUsd: number;
  changePct: number;
  spark: number[];
};

export type DemoPosition = {
  protocol: string;
  title: string;
  detail: string;
  valueUsd: number;
  changePct: number;
  apr: string;
  range: string;
  fees: string;
};

export type DemoStageName = "signed" | "sent" | "landed" | "reported" | "failed";

export type DemoActivity = {
  id: string;
  protocol: string;
  title: string;
  chain: string;
  evidenced: DemoStageName[];
};

export type DemoStrategy = {
  name: string;
  runs: { when: string; result: string }[];
};

const up = (start: number, end: number, n: number) =>
  Array.from({ length: n }, (_, i) => Math.round(start + ((end - start) * i) / (n - 1)));

export const DEMO_ACCOUNT = {
  kind: DEMO_KIND,
  name: "Demo account",
  totalUsd: 12841,
  changePct: 2.4,
  costBasisUsd: 11920,
  pnlUsd: 921,
  nearPrice: 4.4,
  keeper: "Demo keeper",
  pnl: {
    "1D": up(12640, 12841, 8),
    "1W": up(12110, 12841, 7),
    "1M": up(11480, 12841, 12),
    All: up(9800, 12841, 16),
  } satisfies Record<DemoRange, number[]>,
  holdings: [
    { id: "near", symbol: "NEAR", name: "NEAR", chain: "NEAR", amount: "420", valueUsd: 1848, changePct: 1.2, spark: up(4.1, 4.4, 12) },
    { id: "wnear", symbol: "wNEAR", name: "Wrapped NEAR", chain: "NEAR", amount: "180", valueUsd: 792, changePct: 0.4, spark: up(4.2, 4.4, 12) },
    { id: "sol", symbol: "SOL", name: "Solana", chain: "Solana", amount: "18.4", valueUsd: 2640, changePct: -0.8, spark: [148, 151, 146, 144, 143, 145] },
    { id: "usdc", symbol: "USDC", name: "USD Coin", chain: "Solana", amount: "1,200", valueUsd: 1200, changePct: 0, spark: [1, 1, 1, 1, 1, 1] },
    { id: "eth", symbol: "ETH", name: "Ether", chain: "EVM", amount: "0.62", valueUsd: 1860, changePct: 3.1, spark: up(2920, 3000, 10) },
    { id: "nvda", symbol: "NVDA", name: "NVDA", chain: "EVM", amount: "4.1", valueUsd: 4501, changePct: 1.8, spark: up(240, 270, 10) },
  ] satisfies DemoHolding[],
  positions: [
    { protocol: "wnear", title: "Wrapped", detail: "180 NEAR sitting as wNEAR", valueUsd: 792, changePct: 0.4, apr: "—", range: "1:1", fees: "$0" },
    { protocol: "rhea", title: "Rhea LP", detail: "wNEAR / MEME pool", valueUsd: 640, changePct: 1.1, apr: "12.4%", range: "0.8–1.4", fees: "$18.20" },
    { protocol: "linear", title: "LiNEAR stake", detail: "180 NEAR staked", valueUsd: 810, changePct: 0.6, apr: "8.1%", range: "liquid", fees: "$0" },
    { protocol: "intents", title: "Intents hop", detail: "SOL toward ETH", valueUsd: 220, changePct: 0, apr: "—", range: "in flight", fees: "$1.10" },
    { protocol: "meteora", title: "Meteora DLMM", detail: "SOL / USDC", valueUsd: 2100, changePct: -0.4, apr: "18.6%", range: "bins ±25", fees: "$42.00" },
    { protocol: "pump", title: "pump.fun bag", detail: "0.4 SOL of a coin", valueUsd: 86, changePct: 4.2, apr: "—", range: "curve", fees: "$0.40" },
    { protocol: "jupiter", title: "Jupiter quote", detail: "Owner-signed swap. Not a flow node.", valueUsd: 0, changePct: 0, apr: "—", range: "quote", fees: "$0.20" },
    { protocol: "uniswap-v4", title: "Uniswap v4", detail: "ETH / NVDA", valueUsd: 3200, changePct: 2.2, apr: "9.4%", range: "tick 40", fees: "$27.50" },
    { protocol: "stonk", title: "ETHNVDA basket", detail: "Pair-token shares", valueUsd: 1493, changePct: 1.4, apr: "—", range: "zap", fees: "$6.10" },
  ] satisfies DemoPosition[],
  activity: [
    { id: "a1", protocol: "linear", title: "Stake with LiNEAR", chain: "NEAR", evidenced: ["signed", "landed", "reported"] },
    { id: "a2", protocol: "rhea", title: "Rhea LP fee", chain: "NEAR", evidenced: ["signed", "landed", "reported"] },
    { id: "a3", protocol: "meteora", title: "Meteora fee sweep", chain: "Solana", evidenced: ["signed", "landed"] },
    { id: "a4", protocol: "uniswap-v4", title: "Buy NVDA", chain: "EVM", evidenced: ["signed", "landed", "reported"] },
    { id: "a5", protocol: "intents", title: "Intents hop", chain: "NEAR", evidenced: ["signed", "failed"] },
  ] satisfies DemoActivity[],
  strategies: [
    { name: "Three-chain draft", runs: [{ when: "Sample morning", result: "4 steps" }, { when: "Sample evening", result: "2 steps" }] },
    { name: "LiNEAR drip", runs: [{ when: "Sample week", result: "1 stake" }] },
  ] satisfies DemoStrategy[],
};

export const DEMO_RANGES: DemoRange[] = ["1D", "1W", "1M", "All"];

export const DEMO_CARD_IDS = [
  "portfolio.hero",
  "portfolio.holdings",
  "portfolio.activity",
  "portfolio.keeper",
  "chart.price",
  "chart.position",
  "chart.pnl",
  "strategies.runs",
  "strategies.keeper",
  ...DEMO_ACCOUNT.positions.map((position) => `protocol.${position.protocol}`),
] as const;

export type DemoCardId = (typeof DEMO_CARD_IDS)[number];

const UNAVAILABLE = new Set<DemoCardId>(["portfolio.hero", "chart.price", "chart.pnl"]);

/** What a card is allowed to show. Demo numbers exist only while demo is on. */
export function cardFace(demo: boolean, id: DemoCardId): { tag: "Demo" | null; primary: string } {
  if (!demo) {
    return { tag: null, primary: UNAVAILABLE.has(id) ? "Unavailable" : "unknown" };
  }
  if (id === "portfolio.hero" || id === "chart.pnl") return { tag: "Demo", primary: String(DEMO_ACCOUNT.totalUsd) };
  if (id === "chart.position") return { tag: "Demo", primary: String(DEMO_ACCOUNT.costBasisUsd) };
  if (id === "chart.price") return { tag: "Demo", primary: String(DEMO_ACCOUNT.nearPrice) };
  if (id === "portfolio.keeper" || id === "strategies.keeper") return { tag: "Demo", primary: DEMO_ACCOUNT.keeper };
  return { tag: "Demo", primary: "Demo" };
}

export function demoPnlSource(range: DemoRange): string {
  const values = DEMO_ACCOUNT.pnl[range];
  const min = Math.min(...values) - 40;
  const max = Math.max(...values) + 40;
  const labels = values.map((_, index) => String(index + 1)).join(", ");
  return [
    "xychart",
    `  title "Demo PnL ${range}"`,
    `  x-axis "step" [${labels}]`,
    `  y-axis "USD" ${min} --> ${max}`,
    `  line [${values.join(", ")}]`,
  ].join("\n");
}

/** True when a saved flow or export accidentally contains the demo account. */
export function draftContainsDemo(json: string): boolean {
  if (json.includes(DEMO_KIND)) return true;
  if (json.includes(DEMO_ACCOUNT.keeper)) return true;
  if (json.includes(String(DEMO_ACCOUNT.totalUsd))) return true;
  return DEMO_ACCOUNT.holdings.some((holding) => json.includes(`"${holding.amount}"`) && json.includes(holding.symbol) && json.includes(String(holding.valueUsd)));
}
