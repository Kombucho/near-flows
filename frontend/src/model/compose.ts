import type { ChainId, FlowNode } from "./flow";

export type ComposeAction = {
  id: string;
  title: string;
  venue: string;
  service: string;
  input: string;
  output: string | null;
  serializable: boolean;
};

const SOL = "sol:SOL";
const ETH = "evm:4663:eth";
const WNEAR = "wrap.testnet";

export const COMPOSE_ACTIONS: Record<ChainId, Record<string, ComposeAction[]>> = {
  near: {
    wnear: [{ id: "wrap", title: "Wrap NEAR", venue: "wNEAR", service: "wnear", input: "near", output: WNEAR, serializable: true }],
    rhea: [{ id: "rhea", title: "Swap on Rhea", venue: "Rhea", service: "rhea", input: WNEAR, output: "meme.testnet", serializable: true }],
    linear: [{ id: "linear", title: "Stake with LiNEAR", venue: "LiNEAR", service: "linear", input: "near", output: "linear-protocol.testnet", serializable: true }],
    intents: [{ id: "intents", title: "Intents hop", venue: "NEAR Intents", service: "intents", input: WNEAR, output: ETH, serializable: false }],
  },
  solana: {
    meteora: [{ id: "dlmm", title: "Open a Meteora LP", venue: "Meteora DLMM", service: "meteora", input: SOL, output: null, serializable: true }],
    pump: [{ id: "pump", title: "Buy on pump.fun", venue: "pump.fun", service: "pump", input: SOL, output: "sol:mint", serializable: true }],
    jupiter: [{ id: "jup", title: "Jupiter swap", venue: "Jupiter", service: "jupiter", input: SOL, output: null, serializable: false }],
  },
  robinhood: {
    "uniswap-v4": [{ id: "v4", title: "Swap on Uniswap v4", venue: "Uniswap v4", service: "uniswap-v4", input: ETH, output: "evm:4663:nvda", serializable: true }],
    stonk: [{ id: "stonk", title: "Zap into the basket", venue: "ETHNVDA pair token", service: "stonk", input: ETH, output: "evm:4663:ethnvda", serializable: true }],
  },
};

export function actionsFor(chain: ChainId, protocol: string): ComposeAction[] {
  return COMPOSE_ACTIONS[chain][protocol] ?? [];
}

function step(partial: Omit<FlowNode, "params" | "routes" | "trigger" | "position"> & { planAmount?: string; auto?: boolean; y?: number }): FlowNode {
  return {
    ...partial,
    trigger: partial.input === "none" ? { when: "manual" } : { when: "now" },
    routes: [],
    params: partial.planAmount ? [{ label: "Plan amount", value: `${partial.planAmount} NEAR` }] : [],
    position: { x: 80 + partial.index * 300, y: partial.y ?? 180 },
  };
}

function link(nodes: FlowNode[]) {
  for (let i = 0; i < nodes.length - 1; i += 1) {
    const next = nodes[i + 1];
    if (!nodes[i].output || nodes[i].output !== next.input) continue;
    nodes[i].routes = [{ to: next.index, bps: 10_000 }];
  }
  return nodes;
}

/** Plan nodes only. Nothing here is signed or broadcast. */
export function composePlan(input: {
  chain: ChainId;
  protocol: string;
  actionId: string;
  amountNear: string;
  startIndex: number;
  origin?: { x: number; y: number };
}): FlowNode[] {
  const action = actionsFor(input.chain, input.protocol).find((item) => item.id === input.actionId);
  if (!action) return [];
  const amount = input.amountNear.trim();
  let index = input.startIndex;
  const landing = input.chain === "solana" ? SOL : ETH;
  const vaultTitle = input.chain === "solana" ? "Solana vault" : "EVM vault";

  if (input.chain === "near") {
    const nodes: FlowNode[] = [];
    if (action.input === WNEAR) {
      nodes.push(step({
        id: `auto-wrap-${index}`,
        index,
        title: "Wrap NEAR",
        venue: "wNEAR",
        chain: "near",
        service: "wnear",
        input: "near",
        output: WNEAR,
        serializable: true,
        catalogId: "node.wrap_near",
        auto: true,
        planAmount: amount,
      }));
      index += 1;
    }
    nodes.push(step({
      id: `plan-${action.id}-${index}`,
      index,
      title: action.title,
      venue: action.venue,
      chain: "near",
      service: action.service,
      input: action.input,
      output: action.output,
      serializable: action.serializable,
      catalogId: action.serializable ? action.id : "intents-hop",
      planAmount: amount,
    }));
    return link(nodes);
  }

  const wrapIndex = index;
  const hopIndex = index + 1;
  const vaultIndex = index + 2;
  const targetIndex = index + 3;
  const origin = input.origin ?? { x: 80, y: 180 };
  const nodes: FlowNode[] = [
    step({
      id: `auto-wrap-${wrapIndex}`,
      index: wrapIndex,
      title: "Wrap NEAR",
      venue: "wNEAR",
      chain: "near",
      service: "wnear",
      input: "near",
      output: WNEAR,
      serializable: true,
      catalogId: "node.wrap_near",
      auto: true,
      planAmount: amount,
    }),
    step({
      id: `auto-hop-${hopIndex}`,
      index: hopIndex,
      title: "Intents hop",
      venue: "NEAR Intents",
      chain: "near",
      service: "intents",
      input: WNEAR,
      output: landing,
      serializable: false,
      catalogId: "intents-hop",
      auto: true,
      planAmount: amount,
    }),
    step({
      id: `auto-vault-${vaultIndex}`,
      index: vaultIndex,
      title: vaultTitle,
      venue: input.chain === "solana" ? "Solana" : "Robinhood Chain",
      chain: input.chain,
      service: "vault",
      input: landing,
      output: landing,
      serializable: false,
      catalogId: "plan.vault",
      auto: true,
      planAmount: amount,
    }),
    step({
      id: `plan-${action.id}-${targetIndex}`,
      index: targetIndex,
      title: action.title,
      venue: action.venue,
      chain: input.chain,
      service: action.service,
      input: action.input,
      output: action.output,
      serializable: action.serializable,
      catalogId: action.id,
      planAmount: amount,
    }),
  ];
  nodes.forEach((node, index) => {
    node.position = { x: origin.x + index * 300, y: origin.y };
  });
  return link(nodes);
}

export function edgeCaption(node: FlowNode, bps: number): string {
  const token = node.output ?? node.input;
  const name = token === "near" ? "NEAR" : token === "wrap.testnet" ? "wNEAR" : token.endsWith(":eth") ? "ETH" : token.endsWith(":SOL") ? "SOL" : "out";
  if (bps === 10_000 && node.planAmount && node.output === "wrap.testnet") return `${node.planAmount} NEAR`;
  if (bps === 10_000) return `all ${name}`;
  const pct = (bps / 100).toFixed(2).replace(/\.00$/, "");
  return `${pct}% ${name}`;
}

export function draftFromPrompt(text: string): { chain: ChainId; protocol: string; actionId: string } | null {
  const line = text.toLowerCase();
  if (line.includes("meteora") || line.includes("lp")) return { chain: "solana", protocol: "meteora", actionId: "dlmm" };
  if (line.includes("pump")) return { chain: "solana", protocol: "pump", actionId: "pump" };
  if (line.includes("jupiter")) return { chain: "solana", protocol: "jupiter", actionId: "jup" };
  if (line.includes("linear") || line.includes("stake")) return { chain: "near", protocol: "linear", actionId: "linear" };
  if (line.includes("rhea")) return { chain: "near", protocol: "rhea", actionId: "rhea" };
  if (line.includes("nvda") || line.includes("uniswap")) return { chain: "robinhood", protocol: "uniswap-v4", actionId: "v4" };
  if (line.includes("wrap")) return { chain: "near", protocol: "wnear", actionId: "wrap" };
  return null;
}
