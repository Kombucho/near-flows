/**
 * Parser for the narrative lines scripts/demo/run.sh prints.
 * Those lines are prefixed DEMO| by contracts/sandbox/tests/demo.rs.
 * This module does not invent timestamps, balances, or missing stages.
 */

export type Stage = "signed" | "sent" | "landed" | "reported" | "failed";

export type ReplayEvent = {
  id: string;
  line: number;
  raw: string;
  leg: number | null;
  legTitle: string | null;
  /** Stages this one line actually evidences. Others stay not recorded. */
  evidenced: Stage[];
};

export type ReplayLeg = { n: number; title: string; line: number };

export type ReplayDocument = {
  format: "DEMO|";
  hasTimestamps: false;
  events: ReplayEvent[];
  legs: ReplayLeg[];
  warnings: string[];
};

const LEG = /^━━━\s*(\d+)\.\s*(.+?)\s*━━━$/;

export function stagesInLine(text: string): Stage[] {
  const found = new Set<Stage>();
  if (/REFUSED/i.test(text)) found.add("failed");
  if (/\b(signs|signed)\b/i.test(text) && !/nothing signed|not signed/i.test(text)) found.add("signed");
  if (/\bsent\b/i.test(text)) found.add("sent");
  if (/executed on the fork|\bon the fork\b|executed:|buy executed|zap executed|rung \d+ executed/i.test(text)) {
    found.add("landed");
  }
  if (/\breports?\b|\breported\b/i.test(text)) found.add("reported");
  return [...found];
}

export function parseDemoLog(source: string, label = "imported log"): ReplayDocument {
  const warnings: string[] = [];
  if (!source.includes("DEMO|")) {
    warnings.push(`${label} has no DEMO| lines. Nothing was projected onto the graph.`);
  }
  const events: ReplayEvent[] = [];
  const legs: ReplayLeg[] = [];
  let leg: ReplayLeg | null = null;
  const seen = new Set<string>();
  const lines = source.split(/\r?\n/);
  lines.forEach((line, index) => {
    const trimmed = line.trim();
    if (!trimmed.startsWith("DEMO|")) return;
    const raw = trimmed.slice("DEMO|".length).trim();
    if (!raw) return;
    const header = raw.match(LEG);
    if (header) {
      leg = { n: Number(header[1]), title: header[2], line: index + 1 };
      legs.push(leg);
      return;
    }
    const identity = `${index + 1}|${raw}`;
    if (seen.has(identity)) return;
    seen.add(identity);
    events.push({
      id: `L${index + 1}`,
      line: index + 1,
      raw,
      leg: leg?.n ?? null,
      legTitle: leg?.title ?? null,
      evidenced: stagesInLine(raw),
    });
  });
  if (legs.length > 0 && legs.length < 4) {
    warnings.push(`This log has ${legs.length} leg header(s). A four-leg demo would have four. Missing legs stay absent.`);
  }
  return { format: "DEMO|", hasTimestamps: false, events, legs, warnings };
}

export type NodeStage = {
  nodeId: string;
  evidenced: Stage[];
  leg: number;
  sourceLines: number[];
};

/**
 * Associate events with nodes only by leg number, using the headers in demo.rs.
 * The narrative does not carry node ids. Nodes outside the active leg are untouched.
 */
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
      const current = byNode.get(node.id) ?? { nodeId: node.id, evidenced: [], leg: event.leg, sourceLines: [] };
      for (const stage of event.evidenced) {
        if (!current.evidenced.includes(stage)) current.evidenced.push(stage);
      }
      current.sourceLines.push(event.line);
      byNode.set(node.id, current);
    }
  }
  return [...byNode.values()];
}
