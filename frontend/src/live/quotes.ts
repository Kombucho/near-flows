export type Spot = { usd: number; changePct: number | null };
export type LiveBook = {
  status: "live" | "unavailable";
  updatedAt: number | null;
  /** Which public feed produced the spots. Absent when the book is empty. */
  source?: "CoinGecko" | "Ref Finance";
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

export function tokenUnit(token: string | null): { asset: Asset | null; symbol: string } {
  if (!token || token === "none") return { asset: null, symbol: "—" };
  if (token === "near") return { asset: "near", symbol: "NEAR" };
  if (token.includes("wrap")) return { asset: "near", symbol: "wNEAR" };
  if (token.includes("linear")) return { asset: "near", symbol: "LiNEAR" };
  if (token === "sol:SOL" || token.endsWith(":SOL")) return { asset: "sol", symbol: "SOL" };
  if (token.includes("EPj") || token.toLowerCase().includes("usdc")) return { asset: "usdc", symbol: "USDC" };
  if (token.endsWith(":ethnvda")) return { asset: null, symbol: "ETHNVDA" };
  if (token.endsWith(":nvda")) return { asset: null, symbol: "NVDA" };
  if (token.endsWith(":eth")) return { asset: "eth", symbol: "ETH" };
  if (token.includes("meme")) return { asset: null, symbol: "MEME" };
  if (token.startsWith("sol:")) return { asset: null, symbol: "coin" };
  return { asset: null, symbol: "token" };
}

/** Prefer the output when we can price it. Otherwise the input. Never a different asset. */
export function quotedToken(input: string, output: string | null): string {
  const produced = tokenUnit(output);
  if (output && produced.asset) return output;
  const accepted = tokenUnit(input);
  if (accepted.asset) return input;
  return output ?? input;
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

function demoAmount(symbol: string, service: string | undefined, input: string | undefined): string | null {
  if (symbol === "wNEAR" && service === "wnear") return "180 wNEAR";
  if (symbol === "LiNEAR" && service === "linear") return "180 LiNEAR";
  if (symbol === "SOL" && (service === "pump" || service === "meteora" || service === "vault")) return "18.4 SOL";
  if (symbol === "USDC" && service === "meteora") return "1,200 USDC";
  if (symbol === "ETH" && (service === "uniswap-v4" || service === "stonk") && input?.endsWith(":eth")) return "0.62 ETH";
  return null;
}

export function nodeReadout(input: {
  demo: boolean;
  token: string | null;
  planAmount?: string;
  service?: string;
  inputToken?: string;
  /** Demo wrap/entry price for NEAR and wNEAR only, e.g. "4.40". */
  demoEntry?: string;
  book: LiveBook;
  now: number;
}): NodeReadout {
  const unit = tokenUnit(input.token);
  const spot = unit.asset ? input.book[unit.asset] : null;
  const live = input.book.status === "live" && !!spot;
  const seconds = live && input.book.updatedAt ? Math.max(0, Math.round((input.now - input.book.updatedAt) / 1000)) : null;
  const nearFamily = unit.symbol === "NEAR" || unit.symbol === "wNEAR" || unit.symbol === "LiNEAR";
  const sample = input.demo ? demoAmount(unit.symbol, input.service, input.inputToken) : null;
  const amount = input.planAmount && nearFamily
    ? { text: `${input.planAmount} ${unit.symbol}`, tag: "Plan" as const }
    : sample
      ? { text: sample, tag: "Demo" as const }
      : { text: `unknown ${unit.symbol}`, tag: null };
  const entryNumber = input.demo && input.demoEntry && nearFamily ? Number(input.demoEntry) : null;
  const entryKnown = entryNumber != null && Number.isFinite(entryNumber) && entryNumber > 0;
  let change: string | null = "unknown";
  let changeUp: boolean | null = null;
  if (entryKnown && live && spot) {
    const pct = ((spot.usd - entryNumber) / entryNumber) * 100;
    change = `${pct >= 0 ? "+" : ""}${pct.toFixed(1)}%`;
    changeUp = pct >= 0;
  }
  const sparkKey = unit.asset === "sol" ? "sol" : unit.asset === "eth" ? "eth" : unit.asset === "near" ? "near" : null;
  const decimals = unit.asset === "usdc" ? 4 : 2;
  return {
    entry: entryKnown ? input.demoEntry! : "unknown",
    entryTag: entryKnown ? "Demo" : null,
    amount: amount.text,
    amountTag: amount.tag,
    current: live && spot ? `$${spot.usd.toFixed(decimals)} / ${unit.symbol}` : "price unavailable",
    currentTag: live ? "live" : null,
    change,
    changeUp,
    ago: seconds != null ? `live · updated ${seconds}s ago` : "price unavailable",
    status: input.demo ? "demo" : "idle",
    spark: live && sparkKey ? input.book.sparks[sparkKey] ?? [] : [],
  };
}
