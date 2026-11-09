// Remote flow nodes, the keeper's side: what to send for a node whose action is a
// transaction on another chain (an EVM template, a Solana template) or a hop out of
// the vault's NEAR Intents account (brain remote.rs, hop.rs). Pure: no network, so it
// is tested directly (remote.test.ts); the network half is remote-run.ts.
//
// The brain decides everything that matters (amount, minimum, deadline, the EVM nonce
// and fee cap, the hop's destination). The keeper fills only what the brain leaves
// open: a minimum it may tighten (a quote less slippage), a Solana message built with
// the protocol's own SDK, a tip and a gas limit under the owner's caps. A template
// with no builder here is skipped and logged, never guessed.

import { homedir } from "node:os";
import { join } from "node:path";
import { existsSync, mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";

// ---------- the brain's shapes (get_vault) ----------

export type RemoteCall =
  | { chain: "evm"; template: string; amount_leaf?: number | null; min_leaf?: number | null; deadline_leaf?: number | null }
  | { chain: "sol"; template: string; amount_field?: number | null; min_field?: number | null }
  | { chain: "hop" };
export type Remote = { call: RemoteCall; out?: string | null; max_out_per_in?: string | null; max_out?: string | null; price?: { feed: string; quote?: string | null } | null };

export type EvmArg =
  | { rule: "fixed"; value: string }
  | { rule: "vault" }
  | { rule: "at_most"; max: string }
  | { rule: "floor"; of: number; num: string; den: string }
  | { rule: "market"; of: number; num: string; den: string; market: unknown }
  | { rule: "share"; of: number; num: string; den: string; market: unknown; share: { name: string; a: string; b: string; k: string } }
  | { rule: "same"; of: number }
  | { rule: "bytes"; hex: string }
  | { rule: "encoded"; types: string; args: EvmArg[] }
  | { rule: "tuple"; args: EvmArg[] }
  | { rule: "list"; items: EvmArg[] }
  | { rule: "deadline" };
export type EvmTemplate = { name: string; to: string; function: string; args: EvmArg[]; value?: EvmArg | null; spends?: { arg: number; token: string } | null };
export type EvmConfig = { signer: string; chain_id: number; address: string | null; templates: EvmTemplate[]; max_fee: string; max_tip: string; gas_cap: number; next_nonce?: number };

export type SolField = { ty: string; rule: string; of?: number; num?: string; den?: string; max?: string; hex?: string };
export type SolAcc = { acc: string; key?: string; mint?: string; program?: string; mint_at?: number; program_at?: number };
export type SolTemplate = { name: string; program: string; prefix: string; fields: SolField[]; accounts: SolAcc[]; spends?: { field: number; key: string }[] };
export type SolConfig = { signer: string; address: string | null; max_cu_price: number; max_cu_limit: number; templates: SolTemplate[]; owner_wallet?: string | null };

/** A remote step the brain is waiting on (VaultState.remote, get_remote_pending). */
export type Pending = { flow: number; node: number; amount: string; min_out: string; max_out: string; expires_ns: number | null; signed: boolean; hash: string };

// ---------- tokens on other chains ----------

/** "evm:4663:eth" (native), "evm:4663:0x…" (an ERC-20), "sol:SOL", "sol:<mint>". */
export type RemoteToken = { chain: "evm"; chainId: number; token: "eth" | string } | { chain: "sol"; mint: "SOL" | string };

export function parseToken(t: string): RemoteToken | null {
  const p = t.split(":");
  if (p[0] === "evm" && p.length === 3 && /^\d+$/.test(p[1])) {
    if (p[2] === "eth") return { chain: "evm", chainId: Number(p[1]), token: "eth" };
    return /^0x[0-9a-fA-F]{40}$/.test(p[2]) ? { chain: "evm", chainId: Number(p[1]), token: p[2].toLowerCase() } : null;
  }
  if (p[0] === "sol" && p.length === 2 && p[1].length > 0) return { chain: "sol", mint: p[1] };
  return null;
}

// ---------- how much a node spends (the brain's ledger begin_node) ----------

export type Trigger = { when: string; chunk?: string };

/** What the brain will spend when it runs the node now: its inbox, a DCA chunk of it, nothing for a collector. */
export function amountOf(input: string, when: Trigger, inbox: string): bigint {
  if (input === "none") return 0n;
  const all = BigInt(inbox);
  if (when.when === "every" && when.chunk !== undefined) return BigInt(when.chunk) < all ? BigInt(when.chunk) : all;
  return all;
}

// ---------- EVM templates ----------

/** The template's leaves, depth first, the ETH value last: the brain's evm::leaves. */
export function evmLeaves(t: EvmTemplate): EvmArg[] {
  const out: EvmArg[] = [];
  const walk = (a: EvmArg) => {
    if (a.rule === "encoded" || a.rule === "tuple") a.args.forEach(walk);
    else if (a.rule === "list") a.items.forEach(walk);
    else out.push(a);
  };
  t.args.forEach(walk);
  if (t.value) walk(t.value);
  return out;
}

/** The EVM templates the keeper knows how to run as a flow node (nodes/evm-templates.json).
 *  gas: the gas limit it asks for (never over the owner's gas_cap). quote: "v4" tightens the
 *  minimum to the Uniswap v4 quoter's answer less slippage; without one the brain's floor
 *  or market minimum is the minimum. Every other number is the brain's. */
export const EVM_BUILDERS: Record<string, { gas: number; quote?: "v4" }> = {
  // Robinhood Chain: Uniswap v4 ETH <-> NVDA (the quote less slippage, raised by the brain to its market
  // floor from the ETH and NVDA feeds), the ETHNVDA basket zap and Smart LP (the brain's share floor:
  // the share's value from its tokens' feeds and the owner's pinned k).
  v4_swap_eth_nvda: { gas: 1_000_000, quote: "v4" },
  v4_swap_nvda_eth: { gas: 1_000_000, quote: "v4" },
  pairtoken_ethnvda_zap_eth: { gas: 1_000_000 },
  smartlp_amzn_deposit_usdg: { gas: 1_000_000 },
  // Base: Uniswap v3 WETH -> USDC (market minimum from the oracle), Aave supply.
  uniswap_swap_weth_usdc: { gas: 300_000 },
  aave_supply_usdc: { gas: 400_000 },
  // Back into NEAR Intents: to the owner-pinned deposit address only.
  intents_deposit_eth: { gas: 100_000 },
  intents_deposit_usdc: { gas: 100_000 },
  intents_deposit_usdg: { gas: 100_000 },
};

export type Skip = { skip: string };
export type EvmPlan = { kind: "evm"; template: EvmTemplate; leaves: number; minLeaf: number | null; gas: number; quote?: V4Key; spendsEth: boolean };
export type V4Key = { currency0: string; currency1: string; fee: number; tickSpacing: number; hooks: string; zeroForOne: boolean };

/** How to run an EVM remote node, or why the keeper won't. */
export function evmPlan(cfg: EvmConfig | null | undefined, call: Extract<RemoteCall, { chain: "evm" }>): EvmPlan | Skip {
  if (!cfg?.address) return { skip: "the vault has no EVM address (evm_setup)" };
  const b = EVM_BUILDERS[call.template];
  if (!b) return { skip: `no builder for ${call.template}: run it by hand` };
  const t = cfg.templates.find((x) => x.name === call.template);
  if (!t) return { skip: `the vault has no template ${call.template}` };
  const leaves = evmLeaves(t);
  // The keeper may not choose an amount: every at_most leaf is the node's amount or the deadline.
  for (let i = 0; i < leaves.length; i++) {
    if (leaves[i].rule === "at_most" && i !== call.amount_leaf && i !== call.deadline_leaf) return { skip: `${call.template}: leaf ${i} is a number the keeper would have to choose: run it by hand` };
  }
  const minLeaf = call.min_leaf ?? null;
  let quote: V4Key | undefined;
  if (b.quote === "v4") {
    quote = v4Key(leaves) ?? undefined;
    if (!quote || minLeaf === null) return { skip: `${call.template}: not a Uniswap v4 single swap the keeper can quote` };
  }
  const spendsEth = t.value !== undefined && t.value !== null;
  return { kind: "evm", template: t, leaves: leaves.length, minLeaf, gas: Math.min(b.gas, cfg.gas_cap), quote, spendsEth };
}

/** A Universal Router V4_SWAP single swap's pool key and direction (leaves 2-7, see evm-templates.json uniswap_v4.how). */
export function v4Key(leaves: EvmArg[]): V4Key | null {
  const f = (i: number) => (leaves[i]?.rule === "fixed" ? (leaves[i] as { value: string }).value : null);
  if (leaves[0]?.rule !== "bytes" || (leaves[0] as { hex: string }).hex !== "0x10") return null;
  const [c0, c1, fee, ts, hooks, dir] = [f(2), f(3), f(4), f(5), f(6), f(7)];
  if (!c0 || !c1 || !fee || !ts || !hooks || (dir !== "true" && dir !== "false")) return null;
  return { currency0: c0, currency1: c1, fee: Number(fee), tickSpacing: Number(ts), hooks, zeroForOne: dir === "true" };
}

/** One value per leaf: zeros (the brain fills the amount, the deadline and every `same`), the minimum at min_leaf. */
export function evmValues(p: EvmPlan, min: bigint): string[] {
  const v = new Array<string>(p.leaves).fill("0");
  if (p.minLeaf !== null) v[p.minLeaf] = min.toString();
  return v;
}

/** The args of step_remote for an EVM node. The nonce is the brain's next_nonce (a keeper may take
 *  no other) and the fee cap the owner's (the brain signs with it whatever is sent); the tip is
 *  the network's, capped by the owner's. */
export function evmArgs(values: string[], cfg: EvmConfig, tip: bigint, gas: number) {
  const maxTip = BigInt(cfg.max_tip);
  const t = tip < maxTip ? tip : maxTip;
  return { chain: "evm", values, nonce: cfg.next_nonce ?? 0, max_fee: cfg.max_fee, max_tip: t.toString(), gas };
}

/** A quote less `bps` of slippage. */
export const lessSlippage = (q: bigint, bps: number) => (q * BigInt(10_000 - bps)) / 10_000n;

// ---------- Solana templates ----------

const SYSTEM = "11111111111111111111111111111111";
export const USDC_MINT = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";

export type SolPlan =
  | { kind: "pump_buy"; mint: string; lamports: bigint; floor: bigint }
  | { kind: "pump_sell"; mint: string; units: bigint; floor: bigint }
  | { kind: "pay_sol"; to: string; lamports: bigint }
  | { kind: "pay_spl"; to: string; mint: string; units: bigint; decimals: number };

/** The Solana templates the keeper builds (nodes/sol-templates.json), by name: which field the
 *  node's amount must sit in (the brain checks the message spends exactly it). */
export const SOL_BUILDERS: Record<string, { amountField: number }> = {
  pump_buy: { amountField: 1 },
  pump_sell: { amountField: 0 },
  intents_deposit_sol: { amountField: 0 },
  intents_deposit_usdc: { amountField: 0 },
};

/** `amount` x num / den, the template's at_least floor on field `field`. */
export function solFloor(t: SolTemplate, field: number, amount: bigint): bigint {
  const f = t.fields[field];
  if (f?.rule !== "at_least" || f.num === undefined || f.den === undefined) return 0n;
  return (amount * BigInt(f.num)) / BigInt(f.den);
}

export function solPlan(cfg: SolConfig | null | undefined, call: Extract<RemoteCall, { chain: "sol" }>, amount: bigint): SolPlan | Skip {
  if (!cfg?.address) return { skip: "the vault has no Solana address (sol_setup)" };
  const b = SOL_BUILDERS[call.template];
  if (!b) return { skip: `no builder for ${call.template}: run it by hand` };
  const t = cfg.templates.find((x) => x.name === call.template);
  if (!t) return { skip: `the vault has no template ${call.template}` };
  if (call.amount_field !== b.amountField) return { skip: `${call.template}: the node's amount_field ${call.amount_field ?? "none"} isn't ${b.amountField}: run it by hand` };
  const key = (i: number) => (t.accounts[i]?.acc === "fixed" ? t.accounts[i].key ?? null : null);
  switch (call.template) {
    case "pump_buy":
    case "pump_sell": {
      const mint = key(2);
      if (!mint) return { skip: `${call.template}: no pinned mint` };
      return call.template === "pump_buy"
        ? { kind: "pump_buy", mint, lamports: amount, floor: solFloor(t, 0, amount) }
        : { kind: "pump_sell", mint, units: amount, floor: solFloor(t, 1, amount) };
    }
    case "intents_deposit_sol": {
      const to = key(1);
      if (t.program !== SYSTEM || !to) return { skip: "intents_deposit_sol: not the library's System transfer" };
      return { kind: "pay_sol", to, lamports: amount };
    }
    case "intents_deposit_usdc": {
      const to = key(1);
      if (!to) return { skip: "intents_deposit_usdc: no pinned deposit account" };
      return { kind: "pay_spl", to, mint: USDC_MINT, units: amount, decimals: 6 };
    }
  }
  return { skip: `no builder for ${call.template}: run it by hand` };
}

// ---------- what to report ----------

/** What the keeper may report for a measured `delta`: at least the signed minimum (less means it
 *  didn't land, or not all of it: wait, or the owner settles), at most the node's cap. */
export function reportable(delta: bigint, min: bigint, max: bigint): bigint | null {
  if (delta < min) return null;
  return delta > max ? max : delta;
}

// ---------- in-flight steps, kept across restarts ----------

/** stepping: step_remote about to go (nothing signed known yet); signed: the signed bytes are here,
 *  not broadcast yet; sent: broadcast (or a hop: sent by NEAR itself), waiting for it to land;
 *  settle: it can't land (reverted, refused): only the owner can say so on chain. */
export type Status = "stepping" | "signed" | "sent" | "settle";
export type InFlight = {
  vault: string;
  flow: number;
  node: number;
  kind: "evm" | "sol" | "hop";
  chainId?: number;
  /** The node's output token on the other chain (what's measured), or null (nothing comes out). */
  out: string | null;
  /** The vault's own address on that chain. */
  address: string;
  min?: string;
  max?: string;
  status: Status;
  raw?: string; // EVM: the signed transaction
  txHash?: string; // EVM: its hash; Solana: the signature (base58)
  message?: string; // Solana: the message the vault signed (base64)
  signature?: string; // Solana: the vault's signature (hex)
  token?: string; // hop: the Intents token (sol.omft.near…)
  nearTx?: string; // hop: the NEAR transaction (the bridge's withdrawal_hash)
  before?: string; // hop: the vault's balance of `out` there before the hop
  startedMs: number;
  note?: string;
};
export type KeeperState = { inflight: InFlight[] };

export const keyOf = (f: { vault: string; flow: number; node: number }) => `${f.vault}#${f.flow}#${f.node}`;

/** ~/.flows/keeper/<keeper>.json (FLOWS_KEEPER_DIR overrides the folder). Never in the repo. */
export function statePath(keeperId: string, dir = process.env.FLOWS_KEEPER_DIR ?? join(homedir(), ".flows", "keeper")): string {
  return join(dir, `${keeperId.replace(/[^a-zA-Z0-9._-]/g, "_")}.json`);
}

export function loadState(path: string): KeeperState {
  if (!existsSync(path)) return { inflight: [] };
  const s = JSON.parse(readFileSync(path, "utf8")) as KeeperState;
  return { inflight: Array.isArray(s.inflight) ? s.inflight : [] };
}

/** Written whole, then renamed over the old one: a crash leaves the old file or the new one. */
export function saveState(path: string, s: KeeperState) {
  mkdirSync(join(path, ".."), { recursive: true, mode: 0o700 });
  const tmp = `${path}.tmp`;
  writeFileSync(tmp, JSON.stringify(s, null, 2), { mode: 0o600 });
  renameSync(tmp, path);
}

export function upsert(s: KeeperState, f: InFlight): KeeperState {
  const k = keyOf(f);
  return { inflight: [...s.inflight.filter((x) => keyOf(x) !== k), f] };
}

export function drop(s: KeeperState, f: { vault: string; flow: number; node: number }): KeeperState {
  const k = keyOf(f);
  return { inflight: s.inflight.filter((x) => keyOf(x) !== k) };
}

/** What to do with an in-flight step after a restart (or each round), given what the brain
 *  still waits on (null: nothing pending there any more: reported, or settled by the owner).
 *  Never "send again": a step the brain signed is broadcast as signed, never re-stepped. */
export type Next = "drop" | "broadcast" | "wait" | "settle";
export function nextFor(f: InFlight, pending: Pending | null): Next {
  if (!pending) return "drop";
  switch (f.status) {
    case "stepping":
      // Pending on chain but we never saw what was signed. A hop needs nothing more from us
      // (NEAR sent it); a signed transaction we don't hold can't be broadcast by us.
      return f.kind === "hop" && f.before !== undefined ? "wait" : "settle";
    case "signed":
      return "broadcast";
    case "sent":
      return "wait";
    case "settle":
      return "settle";
  }
}

/** A remote step pending on chain that this keeper has no record of (another keeper, or a crash
 *  between the step and the state file): the owner's to settle. */
export function orphans(pending: Pending[], vault: string, s: KeeperState): Pending[] {
  const mine = new Set(s.inflight.filter((f) => f.vault === vault).map(keyOf));
  return pending.filter((p) => !mine.has(keyOf({ vault, flow: p.flow, node: p.node })));
}

// ---------- events in a NEAR outcome ----------

/** Every EVENT_JSON `event`'s data in a transaction's logs (all receipts). */
export function events(logs: string[], event: string): Record<string, any>[] {
  const out: Record<string, any>[] = [];
  for (const l of logs) {
    if (!l.startsWith("EVENT_JSON:")) continue;
    try {
      const v = JSON.parse(l.slice("EVENT_JSON:".length));
      if (v.event === event) out.push(...(v.data ?? []));
    } catch {
      // not ours
    }
  }
  return out;
}

/** The step's own events: what the brain pinned (min, max), and what it signed. */
export function stepResult(logs: string[], flow: number, node: number) {
  const mine = (e: Record<string, any>) => e.flow === undefined || (e.flow === flow && e.node === node);
  const step = events(logs, "remote_step").find(mine);
  return {
    step: step ? { min: String(step.min_out), max: String(step.max_out), hash: String(step.hash), amount: String(step.amount) } : null,
    evmRaw: (events(logs, "evm_signed")[0]?.raw as string | undefined) ?? null,
    solSignature: (events(logs, "sol_signed")[0]?.signature as string | undefined) ?? null,
    hopSent: events(logs, "hop_sent").find(mine) ?? null,
    failed: events(logs, "step_failed").find(mine) ?? events(logs, "hop_failed").find(mine) ?? null,
  };
}

// ---------- EVM ABI bits (the v4 quoter) ----------

const word = (n: bigint) => (((n % (1n << 256n)) + (1n << 256n)) % (1n << 256n)).toString(16).padStart(64, "0");
const addr = (a: string) => a.toLowerCase().replace(/^0x/, "").padStart(64, "0");

/** V4Quoter.quoteExactInputSingle(((c0,c1,fee,tickSpacing,hooks),zeroForOne,exactAmount,hookData)) calldata,
 *  hookData empty. `selector`: its first 4 bytes of keccak (0xaa9d21cb). */
export function v4QuoteCalldata(k: V4Key, amount: bigint, selector = "aa9d21cb"): string {
  // One dynamic tuple argument: its offset, then the tuple (5-word key, bool, amount, bytes offset), then the empty bytes.
  return "0x" + selector + word(32n) + addr(k.currency0) + addr(k.currency1) + word(BigInt(k.fee)) + word(BigInt(k.tickSpacing)) + addr(k.hooks)
    + word(k.zeroForOne ? 1n : 0n) + word(amount) + word(BigInt(8 * 32)) + word(0n);
}

/** balanceOf(address) calldata. */
export const balanceOfCalldata = (who: string) => "0x70a08231" + addr(who);
