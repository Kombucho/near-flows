/**
 * Parser for narrative lines from contracts/sandbox/tests/demo.rs.
 * run.sh prints only lines prefixed DEMO|. Those lines have no timestamps.
 * A stage is recorded only when the line contains a producer phrase below.
 * Every other stage stays absent (not recorded). Display pacing is not source time.
 */

export type Stage = "signed" | "sent" | "landed" | "reported" | "failed";

export type ReplayEvent = {
  id: string;
  /** 1-based row in the imported file. Source order, not a clock. */
  sourceRow: number;
  raw: string;
  leg: number | null;
  legTitle: string | null;
  evidenced: Stage[];
  /** Stages the producer did not emit on this row. */
  notRecorded: Stage[];
};

export type ReplayLeg = { n: number; title: string; sourceRow: number };

export type ReplayDocument = {
  format: "DEMO|";
  hasTimestamps: false;
  timing: "Producer lines have no timestamps. Row order is file order. Playback speed is display pacing only.";
  events: ReplayEvent[];
  legs: ReplayLeg[];
  warnings: string[];
};

const LEG = /^━━━\s*(\d+)\.\s*(.+?)\s*━━━$/;
const ALL_STAGES: Stage[] = ["signed", "sent", "landed", "reported", "failed"];

/**
 * Phrases copied from say! format strings in demo.rs.
 * "sent" has no phrase in that file, so it is never evidenced.
 * "on the fork", "attesters signed", "signed minimum", and "signatures:" are not stage markers.
 */
export function stagesInLine(text: string): Stage[] {
  const found = new Set<Stage>();
  if (text.includes("REFUSED") || /\brefused \(/.test(text)) found.add("failed");
  if (text.includes("the vault checks every byte and signs") || text.includes("checked and signed by the vault")) {
    found.add("signed");
  }
  if (text.includes("executed on the fork") || text.includes("executed:")) found.add("landed");
  if (text.includes("keeper reports") || text.includes("; reported") || text.endsWith(", reported")) found.add("reported");
  return ALL_STAGES.filter((stage) => found.has(stage));
}

export function parseDemoLog(source: string, label = "imported log"): ReplayDocument {
  const warnings: string[] = [];
  const timing = "Producer lines have no timestamps. Row order is file order. Playback speed is display pacing only.";
  if (!source.includes("DEMO|")) {
    warnings.push(`${label} has no DEMO| lines. No stages were created.`);
  }
  if (source.includes("DEMO|") && !source.includes("━━━ Done ━━━")) {
    warnings.push("No Done header. Rows after the file ends were not invented.");
  }
  const events: ReplayEvent[] = [];
  const legs: ReplayLeg[] = [];
  let leg: ReplayLeg | null = null;
  const legCounts = new Map<number, number>();
  source.split(/\r?\n/).forEach((line, index) => {
    const trimmed = line.trim();
    if (!trimmed.startsWith("DEMO|")) return;
    const raw = trimmed.slice("DEMO|".length).trim();
    if (!raw) return;
    const header = raw.match(LEG);
    const sourceRow = index + 1;
    if (header) {
      const n = Number(header[1]);
      legCounts.set(n, (legCounts.get(n) ?? 0) + 1);
      leg = { n, title: header[2], sourceRow };
      legs.push(leg);
      return;
    }
    const evidenced = stagesInLine(raw);
    events.push({
      id: `row-${sourceRow}`,
      sourceRow,
      raw,
      leg: leg?.n ?? null,
      legTitle: leg?.title ?? null,
      evidenced,
      notRecorded: ALL_STAGES.filter((stage) => !evidenced.includes(stage)),
    });
  });
  for (const [n, count] of legCounts) {
    if (count > 1) warnings.push(`Leg ${n} header appears ${count} times. Both rows are kept in file order.`);
  }
  if (legs.length > 0 && new Set(legs.map((item) => item.n)).size < 4) {
    warnings.push(`Distinct leg headers: ${new Set(legs.map((item) => item.n)).size}. Missing legs stay absent.`);
  }
  return { format: "DEMO|", hasTimestamps: false, timing, events, legs, warnings };
}

export type NodeStage = {
  nodeId: string;
  evidenced: Stage[];
  notRecorded: Stage[];
  leg: number;
  sourceRows: number[];
};

/** Leg headers in demo.rs name the chain. The lines do not name node ids. */
export function projectLegs(
  nodes: { id: string; chain: "near" | "solana" | "robinhood"; service: string }[],
  events: ReplayEvent[],
): NodeStage[] {
  const legChain: Record<number, { chain: "near" | "solana" | "robinhood"; service?: string }> = {
    1: { chain: "near" },
    2: { chain: "solana", service: "pump" },
    3: { chain: "solana", service: "meteora" },
    4: { chain: "robinhood" },
  };
  const byNode = new Map<string, NodeStage>();
  for (const event of events) {
    if (event.leg == null || event.evidenced.length === 0) continue;
    const map = legChain[event.leg];
    if (!map) continue;
    for (const node of nodes) {
      if (node.chain !== map.chain) continue;
      if (map.service && node.service !== map.service) continue;
      const current = byNode.get(node.id) ?? {
        nodeId: node.id,
        evidenced: [],
        notRecorded: [...ALL_STAGES],
        leg: event.leg,
        sourceRows: [],
      };
      for (const stage of event.evidenced) {
        if (!current.evidenced.includes(stage)) current.evidenced.push(stage);
      }
      current.notRecorded = ALL_STAGES.filter((stage) => !current.evidenced.includes(stage));
      current.sourceRows.push(event.sourceRow);
      byNode.set(node.id, current);
    }
  }
  return [...byNode.values()];
}
