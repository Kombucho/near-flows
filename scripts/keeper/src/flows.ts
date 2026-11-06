// Which flow steps are due. Pure: no network, so it can be tested directly.
// The vault re-checks everything on chain; this only avoids sending
// transactions that would fail (and cost the keeper gas).

export const MAX_FAILURES = 3;
export const NO_INPUT = "none";
const SCALE = 10n ** 24n;

export type Trigger =
  | { when: "now" }
  | { when: "after"; at_ns: number }
  | { when: "every"; every_ns: number; chunk: string }
  | { when: "price_below"; price: string }
  | { when: "price_above"; price: string }
  | { when: "manual" };

export type PriceSource = { contract: string; method: string; args: string; per: string };
export type FlowNode = { input: string; when: Trigger; action: { label: string; price: PriceSource | null } };
export type NodeState = { inbox: string; running: string; runs: number; last_run_ns: number; failures: number };
/** As the vault returns it. A hidden flow has no `nodes`, only `hidden` fingerprints. */
export type Flow = { name: string; nodes: FlowNode[]; inputs?: string[]; hidden?: string[]; ledger: { nodes: NodeState[]; busy: number | null } };

export type Due = { flow: number; node: number; label: string; price?: { source: PriceSource; below?: bigint; above?: bigint } };

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
      const base = { flow: id, node: i, label: n.action.label };
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
