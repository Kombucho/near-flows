import { PALETTE } from "./draft";
import type { ChainId } from "./flow";

export type ProtocolPin = {
  id: string;
  label: string;
  /** False when the protocol exists only as an owner-signed action, not a flow node. */
  flowNode: boolean;
};

export type ChainPin = {
  id: ChainId;
  label: string;
  protocols: ProtocolPin[];
};

/**
 * Pins the canvas actually offers.
 * Hyperliquid is absent: it is not in the node library or the chain templates.
 * Jupiter is a pin because sol_sign_swap checks Jupiter routes; it is not a flow recipe.
 * EVM is the Robinhood Chain templates in this draft (chain id stays "robinhood").
 */
export const CHAIN_PINS: ChainPin[] = [
  {
    id: "near",
    label: "NEAR",
    protocols: [
      { id: "wnear", label: "wNEAR", flowNode: true },
      { id: "rhea", label: "Rhea", flowNode: true },
      { id: "linear", label: "LiNEAR", flowNode: true },
      { id: "intents", label: "Intents", flowNode: true },
    ],
  },
  {
    id: "solana",
    label: "Solana",
    protocols: [
      { id: "meteora", label: "Meteora", flowNode: true },
      { id: "pump", label: "pump.fun", flowNode: true },
      { id: "jupiter", label: "Jupiter", flowNode: false },
    ],
  },
  {
    id: "robinhood",
    label: "EVM",
    protocols: [
      { id: "uniswap-v4", label: "Uniswap v4", flowNode: true },
      { id: "stonk", label: "Stonk", flowNode: true },
    ],
  },
];

const EXTRA_LABEL: Record<string, string> = { bazaar: "Bazaar" };

export function chainPin(id: ChainId | "all"): ChainPin | null {
  if (id === "all") return null;
  return CHAIN_PINS.find((pin) => pin.id === id) ?? null;
}

export function chainLabel(id: ChainId): string {
  return chainPin(id)?.label ?? id;
}

export type PaletteGroup = {
  id: string;
  label: string;
  flowNode: boolean;
  items: (typeof PALETTE)[number][];
};

/** Protocol groups for the node chip. Lens service narrows the groups. Hidden nodes stay in the document. */
export function paletteGroups(chain: ChainId | "all", service: string | "all"): PaletteGroup[] {
  const chains = chain === "all" ? CHAIN_PINS : CHAIN_PINS.filter((pin) => pin.id === chain);
  const groups: PaletteGroup[] = [];
  for (const pin of chains) {
    const items = PALETTE.filter((item) => item.chain === pin.id);
    const covered = new Set(pin.protocols.map((protocol) => protocol.id));
    for (const protocol of pin.protocols) {
      if (service !== "all" && service !== protocol.id) continue;
      groups.push({
        id: protocol.id,
        label: chain === "all" ? `${pin.label} · ${protocol.label}` : protocol.label,
        flowNode: protocol.flowNode,
        items: items.filter((item) => item.service === protocol.id),
      });
    }
    if (service !== "all") continue;
    const extras = [...new Set(items.filter((item) => !covered.has(item.service)).map((item) => item.service))];
    for (const extra of extras) {
      groups.push({
        id: extra,
        label: chain === "all" ? `${pin.label} · ${EXTRA_LABEL[extra] ?? extra}` : (EXTRA_LABEL[extra] ?? extra),
        flowNode: true,
        items: items.filter((item) => item.service === extra),
      });
    }
  }
  return groups;
}
