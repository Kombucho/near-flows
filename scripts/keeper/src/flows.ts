// Which flow steps are due. Pure: no network, so it can be tested directly.
// The vault re-checks everything on chain; this only avoids sending
// transactions that would fail (and cost the keeper gas).

export const MAX_FAILURES = 3;
export const NO_INPUT = "none";
const SCALE = 10n ** 24n;
/** Where to learn to run your own keeper for free (the brain's fees.rs REPO_URL). */
export const REPO_URL = "https://github.com/Kombucho/near-flows";
/** The NEAR a vault always keeps (brain lib.rs RESERVE: 0.05 NEAR). */
export const RESERVE = 50_000_000_000_000_000_000_000n;
/** yoctoNEAR per byte of storage. */
export const BYTE_COST = 10_000_000_000_000_000_000n;

export type Trigger =
  | { when: "now" }
  | { when: "after"; at_ns: number }
  | { when: "every"; every_ns: number; chunk: string }
  | { when: "price_below"; price: string }
  | { when: "price_above"; price: string }
  | { when: "manual" };

export type PriceSource = { contract: string; method: string; args: string; per: string };
/** `remote`: a node on another chain (run with step_remote; its price trigger reads the vault's oracle). */
export type FlowNode = { input: string; when: Trigger; action: { label: string; price: PriceSource | null; remote?: unknown | null } };
export type NodeState = { inbox: string; running: string; runs: number; last_run_ns: number; failures: number };
/** As the brain returns it. A hidden flow has no `nodes`, only `hidden` fingerprints. */
export type Flow = { name: string; nodes: FlowNode[]; inputs?: string[]; hidden?: string[]; ledger: { nodes: NodeState[]; busy: number | null } };
/** A relay vault's record in the brain (get_vault): flow ids are positions, deleted flows are null. */
export type VaultState = { owner: string; keeper: string | null; busy: unknown | null; flows: (Flow | null)[]; keeper_fee?: string };

export type Due = { flow: number; node: number; label: string; remote?: boolean; price?: { source: PriceSource; below?: bigint; above?: bigint } };

/** The flows a keeper may run in a vault: only if it is the vault's keeper and no step is running. */
export function runnable(v: VaultState | null, keeperId: string): [number, Flow][] {
  if (!v || v.keeper !== keeperId || v.busy !== null) return [];
  return v.flows.flatMap((f, id): [number, Flow][] => (f ? [[id, f]] : []));
}

/** Steps that may run at `nowNs`. Price-triggered ones come back with what to check. */
export function due(flows: [number, Flow][], nowNs: bigint): Due[] {
  const out: Due[] = [];
  for (const [id, f] of flows) {
    if (f.ledger.busy !== null) continue;
    f.nodes.forEach((n, i) => {
      const s = f.ledger.nodes[i];
      const collector = n.input === NO_INPUT;
      if (!collector && s.failures >= MAX_FAILURES) return; // paused: waits for its owner
      if (!collector && BigInt(s.inbox) === 0n) return; // nothing to spend
      const last = BigInt(s.last_run_ns);
      const w = n.when;
      const base: Due = n.action.remote ? { flow: id, node: i, label: n.action.label, remote: true } : { flow: id, node: i, label: n.action.label };
      switch (w.when) {
        case "manual":
          return; // only the owner runs these
        case "now":
          if (collector) return;
          out.push(base);
          return;
        case "after":
          if (nowNs >= BigInt(w.at_ns)) out.push(base);
          return;
        case "every": {
          const first = collector ? last === 0n : s.runs === 0;
          if (first || nowNs >= last + BigInt(w.every_ns)) out.push(base);
          return;
        }
        case "price_below":
        case "price_above":
          // A remote node's trigger is the vault's own oracle feed: the brain reads it.
          if (n.action.remote) return void out.push(base);
          if (!n.action.price) return; // the vault would refuse it anyway
          out.push({ ...base, price: { source: n.action.price, [w.when === "price_below" ? "below" : "above"]: BigInt(w.price) } });
          return;
      }
    });
  }
  return out;
}

/** Price from a price source's raw answer (an amount for `per` units in), scaled 1e24. */
export function priceOf(raw: string, per: string): bigint {
  return (BigInt(raw) * SCALE) / BigInt(per);
}

/** Has a price trigger fired? */
export function fired(d: Due, price: bigint): boolean {
  if (!d.price) return true;
  if (d.price.below !== undefined) return price <= d.price.below;
  if (d.price.above !== undefined) return price >= d.price.above;
  return false;
}

/** A vault's free NEAR as the brain counts it (free_near): its balance less its
 *  storage, the reserve and what its flows hold in NEAR. Never below zero. */
export function freeNear(balance: bigint, storageBytes: bigint | number, allocatedNear: bigint): bigint {
  const f = balance - BigInt(storageBytes) * BYTE_COST - RESERVE - allocatedNear;
  return f > 0n ? f : 0n;
}

/** Should this keeper run steps for a vault charging `fee`? The brain refuses a
 *  keeper run the vault can't pay (no free work), so don't spend gas on it. */
export function canPayKeeper(fee: bigint, free: bigint): boolean {
  return fee === 0n || free >= fee;
}

/** The same pointer the brain's refusal carries. */
export function keeperUnpaid(vault: string, fee: bigint, free: bigint): string {
  return `${vault}: keeper fee unpaid (price ${fee}, free NEAR ${free} yocto): skipped. Top up NEAR, or run your own keeper for free: ${REPO_URL} (AGENTS.md)`;
}
