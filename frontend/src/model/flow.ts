import { LIMITS } from "./limits";

export type ChainId = "near" | "solana" | "robinhood";

export type Trigger =
  | { when: "now" }
  | { when: "after"; atNs: string }
  | { when: "every"; everyNs: string; chunk: string }
  | { when: "price_below"; price: string }
  | { when: "price_above"; price: string }
  | { when: "manual" };

/** Stored route. Matches flow::Route { to, bps }. Index is the full-document index. */
export type FlowRoute = { to: number; bps: number };

export type FlowNode = {
  id: string;
  index: number;
  title: string;
  venue: string;
  chain: ChainId;
  service: string;
  /** Token id the node accepts. "none" is a collector. */
  input: string;
  /** Token handed out, or null when nothing is routed. */
  output: string | null;
  trigger: Trigger;
  routes: FlowRoute[];
  /** Draft configuration text. Not an observed balance. */
  params: { label: string; value: string }[];
  position: { x: number; y: number };
  /**
   * False for a UI-only node such as the Intents hop. It stays in the local draft.
   * Contract serialization must refuse the whole document and name the node.
   */
  serializable: boolean;
  catalogId?: string;
};

export type FlowDocument = {
  id: string;
  vaultId: string;
  name: string;
  nodes: FlowNode[];
};

export type FlowIssue = {
  code: string;
  nodeId?: string;
  message: string;
};

export type Lens = { chain: ChainId | "all"; service: string | "all" };

const DECIMAL = /^[0-9]+$/;

export function isCollector(node: FlowNode): boolean {
  return node.input === "none";
}

export function routeSum(node: FlowNode): number {
  return node.routes.reduce((sum, route) => sum + route.bps, 0);
}

/** Unallocated output. Not stored as a route. Null when the node has no output. */
export function remainderBps(node: FlowNode): number | null {
  if (!node.output) return null;
  return LIMITS.maxBps - routeSum(node);
}

export function validateFlow(nodes: FlowNode[]): FlowIssue[] {
  const issues: FlowIssue[] = [];
  if (nodes.length === 0) {
    issues.push({ code: "Empty", message: "A flow needs at least one node." });
    return issues;
  }
  if (nodes.length > LIMITS.maxNodes) {
    issues.push({
      code: "TooManyNodes",
      message: `This commit allows at most ${LIMITS.maxNodes} nodes.`,
    });
  }
  const byIndex = new Map(nodes.map((node) => [node.index, node]));
  for (const node of nodes) {
    if (!node.serializable) {
      issues.push({
        code: "UnsupportedNode",
        nodeId: node.id,
        message: `${node.title} (id ${node.id}, index ${node.index}) has no contract recipe on this commit. Contract serialization is blocked. The local draft still includes it.`,
      });
    }
    if (isCollector(node) && node.trigger.when !== "every" && node.trigger.when !== "manual") {
      issues.push({
        code: "BadTrigger",
        nodeId: node.id,
        message: "A collector (input none) can only use every or manual.",
      });
    }
    if (node.trigger.when === "every") {
      let every = 0n;
      try {
        every = BigInt(node.trigger.everyNs);
      } catch {
        every = -1n;
      }
      if (every < LIMITS.minEveryNs || !DECIMAL.test(node.trigger.chunk) || node.trigger.chunk === "0") {
        issues.push({
          code: "BadTrigger",
          nodeId: node.id,
          message: "every requires an interval of at least 60 seconds and a positive decimal chunk.",
        });
      }
    }
    if (
      (node.trigger.when === "price_above" || node.trigger.when === "price_below") &&
      (!DECIMAL.test(node.trigger.price) || node.trigger.price === "0")
    ) {
      issues.push({
        code: "BadTrigger",
        nodeId: node.id,
        message: "A price trigger needs a positive integer in the flow price scale.",
      });
    }
    if (node.routes.length > LIMITS.maxRoutes) {
      issues.push({
        code: "TooManyRoutes",
        nodeId: node.id,
        message: `At most ${LIMITS.maxRoutes} stored routes leave one node.`,
      });
    }
    if (node.routes.length > 0 && !node.output) {
      issues.push({
        code: "NothingToRoute",
        nodeId: node.id,
        message: "This node declares no output, so it cannot store a route.",
      });
    }
    let total = 0;
    for (const route of node.routes) {
      total += route.bps;
      if (!Number.isInteger(route.bps) || route.bps < 0) {
        issues.push({ code: "BadBps", nodeId: node.id, message: "Basis points must be a non-negative integer." });
      }
      if (route.to <= node.index) {
        issues.push({
          code: "RouteBackwards",
          nodeId: node.id,
          message: `Route to index ${route.to} does not point forward.`,
        });
      }
      const target = byIndex.get(route.to);
      if (!target) {
        issues.push({ code: "NoSuchNode", nodeId: node.id, message: `No node at index ${route.to}.` });
        continue;
      }
      if (!node.output || target.input !== node.output || target.input === "none") {
        issues.push({
          code: "RouteTokenMismatch",
          nodeId: node.id,
          message: `Index ${node.index} outputs ${node.output ?? "nothing"}; index ${target.index} accepts ${target.input}.`,
        });
      }
    }
    if (total > LIMITS.maxBps) {
      issues.push({
        code: "RoutesOverfull",
        nodeId: node.id,
        message: `Routes sum to ${total} bps. The maximum is ${LIMITS.maxBps}.`,
      });
    }
  }
  return issues;
}

export function visibleNodes(nodes: FlowNode[], lens: Lens): FlowNode[] {
  return nodes.filter((node) => {
    if (lens.chain !== "all" && node.chain !== lens.chain) return false;
    if (lens.service !== "all" && node.service !== lens.service) return false;
    return true;
  });
}

/** Apply edits made while a lens is on, without dropping hidden nodes or their routes. */
export function mergeLensEdit(full: FlowNode[], lens: Lens, editedVisible: FlowNode[]): FlowNode[] {
  const edits = new Map(editedVisible.map((node) => [node.id, node]));
  return full.map((node) => {
    const shown = visibleNodes([node], lens).length === 1;
    if (!shown) return node;
    return edits.get(node.id) ?? node;
  });
}

export type SerializedNode = {
  index: number;
  input: string;
  output: string | null;
  when: Trigger;
  next: FlowRoute[];
  catalogId?: string;
  title: string;
};

export type LocalDraftFile = {
  kind: "near-flows-local-draft";
  version: 1;
  note: "Local draft. Not a contract call. UI-only nodes are kept.";
  flow: FlowDocument;
};

/** Browser/file persistence. Every node is kept, including UI-only nodes and nodes a lens hides. */
export function exportLocalDraft(flow: FlowDocument): string {
  const file: LocalDraftFile = {
    kind: "near-flows-local-draft",
    version: 1,
    note: "Local draft. Not a contract call. UI-only nodes are kept.",
    flow,
  };
  return JSON.stringify(file);
}

export function importLocalDraft(text: string): FlowDocument {
  const parsed = JSON.parse(text) as Partial<LocalDraftFile>;
  if (parsed.kind !== "near-flows-local-draft" || parsed.version !== 1 || !parsed.flow || !Array.isArray(parsed.flow.nodes)) {
    throw new Error("not a local draft");
  }
  return parsed.flow;
}

export type ContractBlock = {
  id: string;
  index: number;
  title: string;
  catalogId?: string;
  reason: string;
};

/** Contract payload. Refuses the document when any node cannot be a recipe. Does not omit those nodes. */
export function contractSerialize(nodes: FlowNode[]): { ok: true; nodes: SerializedNode[] } | { ok: false; blocked: ContractBlock[] } {
  const blocked = nodes
    .filter((node) => !node.serializable)
    .map((node) => ({
      id: node.id,
      index: node.index,
      title: node.title,
      catalogId: node.catalogId,
      reason: "No flow recipe on this commit. intents_sign_swap is a separate owner call, not this node, and a keeper is not offered that call.",
    }));
  if (blocked.length > 0) return { ok: false, blocked };
  return {
    ok: true,
    nodes: nodes.map((node) => ({
      index: node.index,
      input: node.input,
      output: node.output,
      when: node.trigger,
      next: node.routes.map((route) => ({ to: route.to, bps: route.bps })),
      catalogId: node.catalogId,
      title: node.title,
    })),
  };
}

export function proposeRoute(
  nodes: FlowNode[],
  sourceId: string,
  targetId: string,
  bps: number,
): { ok: true; nodes: FlowNode[] } | { ok: false; code: string; message: string } {
  const source = nodes.find((node) => node.id === sourceId);
  const target = nodes.find((node) => node.id === targetId);
  if (!source || !target) return { ok: false, code: "NoSuchNode", message: "One end of the route is not in the document." };
  if (!Number.isInteger(bps) || bps <= 0) return { ok: false, code: "BadBps", message: "Basis points must be a positive integer." };
  if (target.index <= source.index) {
    return { ok: false, code: "RouteBackwards", message: `${source.title} (index ${source.index}) cannot route backward to ${target.title} (index ${target.index}).` };
  }
  if (!source.output || target.input !== source.output || target.input === "none") {
    return {
      ok: false,
      code: "RouteTokenMismatch",
      message: `${source.title} outputs ${source.output ?? "nothing"}; ${target.title} accepts ${target.input}.`,
    };
  }
  if (source.routes.length >= LIMITS.maxRoutes) {
    return { ok: false, code: "TooManyRoutes", message: `${source.title} already has ${LIMITS.maxRoutes} routes.` };
  }
  if (source.routes.some((route) => route.to === target.index)) {
    return { ok: false, code: "DuplicateRoute", message: `${source.title} already routes to index ${target.index}.` };
  }
  const room = LIMITS.maxBps - routeSum(source);
  if (bps > room) {
    return { ok: false, code: "RoutesOverfull", message: `Only ${room} bps remain on ${source.title}. ${bps} was not stored.` };
  }
  return {
    ok: true,
    nodes: nodes.map((node) => (node.id === source.id ? { ...node, routes: [...node.routes, { to: target.index, bps }] } : node)),
  };
}
