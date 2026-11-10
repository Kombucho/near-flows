import { DEMO_ACCOUNT } from "../demo/fixtures";
import type { FlowNode } from "../model/flow";
import type { ChainId } from "../model/flow";
import { nodeReadout, quotedToken, tokenUnit, type LiveBook } from "../live/quotes";
import type { Stage } from "../model/replay";

/** CSS class suffix. Colours live in styles.css and come from the verified token table. */
export type ThemeId =
  | "near"
  | "intents"
  | "rhea"
  | "met"
  | "jup"
  | "uni"
  | "hl"
  | "sol"
  | "evm"
  | "linear"
  | "neutral";

const ORDER: Stage[] = ["signed", "sent", "landed", "reported", "failed"];

export function themeId(service: string, chain: ChainId): ThemeId {
  if (service === "wnear") return "near";
  if (service === "intents") return "intents";
  if (service === "rhea") return "rhea";
  if (service === "meteora") return "met";
  if (service === "jupiter") return "jup";
  if (service === "uniswap-v4") return "uni";
  if (service === "hyperliquid") return "hl";
  if (service === "linear") return "linear";
  if (service === "vault") {
    if (chain === "solana") return "sol";
    if (chain === "robinhood") return "evm";
    return "near";
  }
  if (service === "pump" || service === "stonk") return "neutral";
  if (chain === "solana") return "sol";
  if (chain === "robinhood") return "evm";
  return "neutral";
}

export function themeClass(service: string, chain: ChainId): string {
  return `t-${themeId(service, chain)}`;
}

export function typeTag(service: string): string {
  if (service === "wnear") return "Wrap";
  if (service === "meteora") return "LP";
  if (service === "linear") return "Stake";
  if (service === "intents") return "Bridge";
  if (service === "vault") return "Vault";
  if (service === "stonk") return "Basket";
  if (service === "pump") return "Buy";
  if (service === "uniswap-v4" || service === "rhea" || service === "jupiter") return "Swap";
  return "Node";
}

export function serialOf(index: number): string {
  const n = Math.max(0, Math.trunc(index)) + 1;
  return `NF ${String(n).padStart(3, "0")}`;
}

export function quoteProvider(book: LiveBook): "CoinGecko" | "Ref Finance" | null {
  if (book.source) return book.source;
  if (book.status !== "live") return null;
  if (book.sol || book.eth || book.usdc) return "CoinGecko";
  if (book.near) return "Ref Finance";
  return null;
}

function shortToken(token: string): string {
  if (token === "none") return "collector";
  if (token === "near") return "NEAR";
  if (token.startsWith("evm:")) return token.split(":").at(-1)?.toUpperCase() ?? token;
  if (token.startsWith("sol:")) {
    const mint = token.slice(4);
    if (mint === "SOL") return "SOL";
    if (mint.startsWith("EPj")) return "USDC";
    return `${mint.slice(0, 4)}…`;
  }
  if (token.includes("wrap")) return "wNEAR";
  if (token.includes("linear")) return "LiNEAR";
  if (token.includes("meme")) return "MEME";
  return token.length > 16 ? `${token.slice(0, 14)}…` : token;
}

function chainName(chain: ChainId): string {
  if (chain === "near") return "NEAR";
  if (chain === "solana") return "Solana";
  return "EVM";
}

function kindName(service: string): string {
  if (service === "wnear") return "Wrap";
  if (service === "meteora") return "Liquidity";
  if (service === "vault") return "Vault";
  if (service === "intents") return "Bridge";
  if (service === "linear") return "Stake";
  if (service === "stonk") return "Basket";
  if (service === "pump") return "Buy";
  return "Swap";
}

function verb(node: FlowNode): string {
  if (node.title.toLowerCase().startsWith("sweep")) return "Sweep";
  if (node.service === "wnear") return "Wrap";
  if (node.service === "meteora") return "Provide";
  if (node.service === "linear") return "Stake";
  if (node.service === "intents") return "Hop";
  if (node.service === "vault") return "Hold";
  if (node.service === "pump") return "Buy";
  if (node.service === "stonk") return "Zap";
  if (node.title.toLowerCase().startsWith("sell")) return "Sell";
  return "Swap";
}

function pairOf(node: FlowNode): string {
  const inn = shortToken(node.input);
  const out = node.output ? shortToken(node.output) : null;
  if (!out || out === inn) return inn;
  return `${inn}→${out}`;
}

function signedPct(value: number): string {
  const body = `${Math.abs(value).toFixed(1)}%`;
  return value >= 0 ? `+${body}` : `−${body}`;
}

function money(usd: number): string {
  const digits = usd >= 100 ? 0 : 2;
  return `$${usd.toLocaleString("en-US", { minimumFractionDigits: digits, maximumFractionDigits: digits })}`;
}

function asUnknown(text: string): { text: string; unknown: boolean } {
  if (text === "price unavailable" || text === "unknown" || text.startsWith("unknown")) {
    return { text: text === "price unavailable" ? "price unavailable" : "unknown", unknown: true };
  }
  return { text, unknown: false };
}

function flavor(node: FlowNode): string {
  if (node.service === "wnear") return "1 NEAR becomes 1 wNEAR. The quote is NEAR's USD price from CoinGecko, with Ref as fallback.";
  if (node.service === "intents") return "A hop through NEAR Intents. It stays on this canvas. Nothing is signed from here.";
  if (node.service === "rhea") return "A Rhea swap of the incoming token. The quote is that token's USD price when a feed exists.";
  if (node.service === "linear") return "Stake NEAR as LiNEAR. The quote is NEAR's USD price when the feed is live.";
  if (node.service === "meteora") return "Spot bins around the active price. An APR is shown only from the labelled demo.";
  if (node.service === "pump") return "A pump.fun buy of the landed token. The amount is a plan or the labelled demo.";
  if (node.service === "jupiter") return "Jupiter is not a flow node. SOL's USD price shows when the feed is live.";
  if (node.service === "uniswap-v4") return "Swap the incoming token on Uniswap v4, then pass the output on.";
  if (node.service === "stonk") return "A zap into the basket. No live basket price is connected.";
  if (node.service === "vault") return "The vault on the destination chain. It holds what the hop delivers.";
  return "A step in this strategy. Missing figures stay unknown.";
}

export type CardFace = {
  theme: string;
  themeId: ThemeId;
  stage: string;
  name: string;
  value: string;
  valueUnknown: boolean;
  chain: ChainId;
  stamp: "DEMO" | null;
  typeLine: string;
  verb: string;
  pair: string;
  costs: 1 | 2;
  damage: string;
  damageLabel: string;
  damageUnknown: boolean;
  damageUp: boolean | null;
  stale: boolean;
  flavor: string;
  entry: string;
  entryUnknown: boolean;
  amount: string;
  amountUnknown: boolean;
  change: string;
  changeUnknown: boolean;
  changeUp: boolean | null;
  source: string;
  reporter: string;
  stages: { name: Stage; failed: boolean }[];
  serial: string;
  demo: boolean;
  attribution: string | null;
};

/** The labelled demo position belongs to one node of that protocol, not every copy. */
export function positionOwner(nodes: FlowNode[]): Set<string> {
  const byService = new Map<string, FlowNode[]>();
  for (const node of nodes) {
    const list = byService.get(node.service) ?? [];
    list.push(node);
    byService.set(node.service, list);
  }
  const owners = new Set<string>();
  for (const list of byService.values()) {
    const position = DEMO_ACCOUNT.positions.find((item) => item.protocol === list[0].service);
    if (!position) continue;
    const needle = position.title.toLowerCase();
    const titled = list.find((node) => node.title.toLowerCase().includes(needle) || needle.includes(node.title.toLowerCase()));
    const winner = titled ?? list.reduce((best, node) => (node.index < best.index ? node : best));
    owners.add(winner.id);
  }
  return owners;
}

export function presentCard(input: {
  node: FlowNode;
  demo: boolean;
  book: LiveBook;
  now: number;
  evidenced?: Stage[];
  /** When false, this node does not wear the protocol's sample position. */
  usePosition?: boolean;
}): CardFace {
  const { node, demo, book, now } = input;
  const readout = nodeReadout({
    demo,
    token: quotedToken(node.input, node.output),
    service: node.service,
    inputToken: node.input,
    planAmount: node.planAmount,
    demoEntry: demo ? DEMO_ACCOUNT.nearPrice.toFixed(2) : undefined,
    book,
    now,
  });
  const unit = tokenUnit(quotedToken(node.input, node.output));
  const spot = unit.asset ? book[unit.asset] : null;
  const live = book.status === "live" && !!spot;
  const position = demo && input.usePosition !== false
    ? DEMO_ACCOUNT.positions.find((item) => item.protocol === node.service)
    : undefined;
  const provider = quoteProvider(book);
  const seconds = live && book.updatedAt != null ? Math.max(0, Math.round((now - book.updatedAt) / 1000)) : null;
  const stale = seconds != null && seconds > 60;
  const age = seconds == null ? null : stale ? `stale ${Math.max(1, Math.round(seconds / 60))}m` : `${seconds}s ago`;

  const yieldNode = node.service === "meteora" || node.service === "linear" || node.service === "rhea";
  let damage = "unknown";
  let damageLabel = "quote";
  let damageUnknown = true;
  let damageUp: boolean | null = null;
  let demoQuote = false;

  if (yieldNode && position && position.apr !== "—") {
    damage = position.apr;
    damageLabel = "DEMO · APR";
    damageUnknown = false;
    demoQuote = true;
  } else if (yieldNode) {
    damage = "unknown";
    damageLabel = "APR";
  } else if (live && spot && (node.service === "uniswap-v4" || node.service === "jupiter" || node.service === "pump" || node.service === "stonk") && spot.changePct != null) {
    damage = signedPct(spot.changePct);
    damageLabel = stale ? `stale · 24h` : "LIVE · 24h";
    damageUnknown = false;
    damageUp = spot.changePct >= 0;
  } else if (live && spot) {
    damage = money(spot.usd);
    damageLabel = `${stale ? "stale" : "LIVE"} · ${unit.symbol}`;
    damageUnknown = false;
  } else if (demo && position && !yieldNode && position.changePct !== 0) {
    damage = signedPct(position.changePct);
    damageLabel = "DEMO · 24h";
    damageUnknown = false;
    damageUp = position.changePct >= 0;
    demoQuote = true;
  } else if (!live) {
    damage = "price unavailable";
    damageLabel = "quote";
  }

  const valueKnown = !!position && position.valueUsd > 0;
  const entryRaw = asUnknown(readout.entry);
  const entry = entryRaw.unknown ? entryRaw : { text: readout.entry.startsWith("$") ? readout.entry : `$${readout.entry}`, unknown: false };
  const amount = asUnknown(readout.amount);
  let change = asUnknown(readout.change ?? "unknown");
  let changeUp = change.unknown ? null : readout.changeUp;
  if (change.unknown && position) {
    change = { text: signedPct(position.changePct), unknown: false };
    changeUp = position.changePct >= 0;
  }

  const evidenced = input.evidenced ?? [];
  const stages = ORDER.filter((name) => evidenced.includes(name)).map((name) => ({ name, failed: name === "failed" }));

  return {
    theme: themeClass(node.service, node.chain),
    themeId: themeId(node.service, node.chain),
    stage: typeTag(node.service),
    name: node.title,
    value: valueKnown ? money(position!.valueUsd) : "unknown",
    valueUnknown: !valueKnown,
    chain: node.chain,
    stamp: demo ? "DEMO" : null,
    typeLine: `${kindName(node.service)} node · ${chainName(node.chain)} · ${node.venue}`,
    verb: verb(node),
    pair: pairOf(node),
    costs: node.service === "wnear" || node.service === "vault" ? 1 : 2,
    damage,
    damageLabel,
    damageUnknown,
    damageUp,
    stale,
    flavor: flavor(node),
    entry: entry.text,
    entryUnknown: entry.unknown,
    amount: amount.text,
    amountUnknown: amount.unknown,
    change: change.text,
    changeUnknown: change.unknown,
    changeUp,
    source: demoQuote ? "labelled demo" : provider && age ? `quote: ${provider} · ${age}` : "price unavailable",
    reporter: demo ? "Demo keeper" : "no account linked",
    stages,
    serial: serialOf(node.index),
    demo,
    attribution: node.service === "uniswap-v4" ? "Uniswap is a trademark of Uniswap Labs." : null,
  };
}
