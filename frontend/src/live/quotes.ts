export type Spot = { usd: number; changePct: number | null };
export type LiveBook = {
  status: "live" | "unavailable";
  updatedAt: number | null;
  near: Spot | null;
  sol: Spot | null;
  eth: Spot | null;
  usdc: Spot | null;
  sparks: { near: number[]; sol: number[]; eth: number[] };
};

export const EMPTY_BOOK: LiveBook = {
  status: "unavailable",
  updatedAt: null,
  near: null,
  sol: null,
  eth: null,
  usdc: null,
  sparks: { near: [], sol: [], eth: [] },
};

type GeckoRow = { usd?: number; usd_24h_change?: number };

export function parseGecko(body: unknown): Pick<LiveBook, "near" | "sol" | "eth" | "usdc"> | null {
  if (!body || typeof body !== "object") return null;
  const row = body as Record<string, GeckoRow>;
  const spot = (id: string): Spot | null => {
    const usd = row[id]?.usd;
    if (typeof usd !== "number" || !Number.isFinite(usd) || usd <= 0) return null;
    const change = row[id]?.usd_24h_change;
    return { usd, changePct: typeof change === "number" && Number.isFinite(change) ? change : null };
  };
  const near = spot("near");
  const sol = spot("solana");
  const eth = spot("ethereum");
  const usdc = spot("usd-coin");
  if (!near && !sol && !eth) return null;
  return { near, sol, eth, usdc };
}

export function parseRefNear(body: unknown): number | null {
  if (!body || typeof body !== "object") return null;
  const row = (body as Record<string, { price?: string }>)["wrap.near"];
  const price = Number(row?.price);
  return Number.isFinite(price) && price > 0 ? price : null;
}

export function parseGeckoChart(body: unknown): number[] {
  if (!body || typeof body !== "object") return [];
  const prices = (body as { prices?: unknown }).prices;
  if (!Array.isArray(prices)) return [];
  return prices
    .map((point) => (Array.isArray(point) ? point[1] : null))
    .filter((value): value is number => typeof value === "number" && Number.isFinite(value) && value > 0)
    .slice(-32);
}

export type PoolRow = { address: string; name: string; apy: number | null; tvl: number | null; price: number | null };

export function parseMeteoraPools(body: unknown): PoolRow[] {
  if (!body || typeof body !== "object") return [];
  const data = (body as { data?: unknown }).data;
  if (!Array.isArray(data)) return [];
  return data.slice(0, 8).flatMap((item) => {
    if (!item || typeof item !== "object") return [];
    const row = item as { address?: string; name?: string; apy?: number; tvl?: number; current_price?: number };
    if (!row.address || !row.name) return [];
    return [{
      address: row.address,
      name: row.name,
      apy: typeof row.apy === "number" ? row.apy : null,
      tvl: typeof row.tvl === "number" ? row.tvl : null,
      price: typeof row.current_price === "number" ? row.current_price : null,
    }];
  });
}

export type Asset = "near" | "sol" | "eth" | "usdc";

export function assetFor(token: string | null): Asset | null {
  if (!token) return null;
  if (token === "near" || token.includes("wrap") || token.includes("linear")) return "near";
  if (token.startsWith("sol:SOL") || token === "sol:SOL") return "sol";
  if (token.includes("usdc") || token.includes("EPj")) return "usdc";
  if (token.endsWith(":eth") || token.endsWith(":ethnvda")) return "eth";
  return null;
}

export type NodeReadout = {
  entry: string;
  entryTag: "Demo" | null;
  amount: string;
  amountTag: "Demo" | "Plan" | null;
  current: string;
  currentTag: "live" | null;
  change: string | null;
  changeUp: boolean | null;
  ago: string;
  status: "demo" | "idle" | "running";
  spark: number[];
};

export function nodeReadout(input: {
  demo: boolean;
  token: string | null;
  planAmount?: string;
  demoAmount?: string;
  demoEntry?: string;
  book: LiveBook;
  now: number;
}): NodeReadout {
  const asset = assetFor(input.token);
  const spot = asset ? input.book[asset] : null;
  const live = input.book.status === "live" && !!spot;
  const seconds = live && input.book.updatedAt ? Math.max(0, Math.round((input.now - input.book.updatedAt) / 1000)) : null;
  const amount = input.planAmount
    ? { text: `${input.planAmount} NEAR`, tag: "Plan" as const }
    : input.demo && input.demoAmount
      ? { text: input.demoAmount, tag: "Demo" as const }
      : { text: "unknown", tag: null };
  const sparkKey = asset === "sol" ? "sol" : asset === "eth" ? "eth" : "near";
  return {
    entry: input.demo && input.demoEntry && asset === "near" ? input.demoEntry : "unknown",
    entryTag: input.demo && input.demoEntry && asset === "near" ? "Demo" : null,
    amount: amount.text,
    amountTag: amount.tag,
    current: live && spot ? spot.usd.toFixed(asset === "usdc" ? 4 : 2) : "price unavailable",
    currentTag: live ? "live" : null,
    change: live && spot?.changePct != null ? `${spot.changePct >= 0 ? "+" : ""}${spot.changePct.toFixed(1)}%` : null,
    changeUp: live && spot?.changePct != null ? spot.changePct >= 0 : null,
    ago: seconds != null ? `live · updated ${seconds}s ago` : "price unavailable",
    status: input.demo ? "demo" : "idle",
    spark: live ? input.book.sparks[sparkKey] ?? [] : [],
  };
}
