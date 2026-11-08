import { createContext, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { boardDocument, nodeFromPalette, PALETTE } from "./model/draft";
import type { FlowDocument, FlowNode, Lens } from "./model/flow";
import { mergeLensEdit } from "./model/flow";
import type { ReplayDocument } from "./model/replay";

export type VaultDraft = {
  id: string;
  label: string;
  kind: "named" | "stealth";
  loginLabel: string;
};

type AppState = {
  loginLabel: string;
  vaults: VaultDraft[];
  selectedVaultId: string;
  flows: FlowDocument[];
  selectedFlowId: string;
  lens: Lens;
  selectedNodeId: string | null;
  replay: ReplayDocument | null;
  replayCursor: number;
  replayPlaying: boolean;
  replaySpeed: 0.5 | 1 | 2;
};

const STORAGE = "near-flows.draft.v1";

function seed(): AppState {
  const vaults: VaultDraft[] = [
    { id: "local-named", label: "Named draft", kind: "named", loginLabel: "local session" },
    { id: "local-stealth", label: "Stealth draft", kind: "stealth", loginLabel: "local session" },
  ];
  const flow = boardDocument(vaults[0].id);
  return {
    loginLabel: "local session",
    vaults,
    selectedVaultId: vaults[0].id,
    flows: [flow],
    selectedFlowId: flow.id,
    lens: { chain: "all", service: "all" },
    selectedNodeId: "nvda",
    replay: null,
    replayCursor: 0,
    replayPlaying: false,
    replaySpeed: 1,
  };
}

function load(): AppState {
  try {
    const raw = localStorage.getItem(STORAGE);
    if (!raw) return seed();
    const parsed = JSON.parse(raw) as AppState;
    if (!parsed.vaults?.length || !parsed.flows?.length) return seed();
    return { ...seed(), ...parsed, replay: null, replayCursor: 0, replayPlaying: false };
  } catch {
    return seed();
  }
}

type Api = AppState & {
  selectVault: (id: string) => void;
  addVault: (vault: VaultDraft) => void;
  selectFlow: (id: string) => void;
  setLens: (lens: Lens) => void;
  selectNode: (id: string | null) => void;
  updateVisibleNode: (node: FlowNode) => void;
  moveNode: (id: string, position: { x: number; y: number }) => void;
  addFromPalette: (catalogId: string) => void;
  setReplay: (doc: ReplayDocument | null) => void;
  setCursor: (cursor: number) => void;
  setPlaying: (playing: boolean) => void;
  setSpeed: (speed: 0.5 | 1 | 2) => void;
  resetReplay: () => void;
};

const Ctx = createContext<Api | null>(null);

export function AppStateProvider({ children }: { children: ReactNode }) {
  const [state, setState] = useState<AppState>(load);

  useEffect(() => {
    const { replay: _replay, replayCursor: _cursor, replayPlaying: _playing, ...persisted } = state;
    localStorage.setItem(STORAGE, JSON.stringify(persisted));
  }, [state]);

  const api = useMemo<Api>(() => {
    const shown = (node: FlowNode, lens: Lens) =>
      (lens.chain === "all" || node.chain === lens.chain) && (lens.service === "all" || node.service === lens.service);
    return {
      ...state,
      selectVault: (id) => setState((s) => ({ ...s, selectedVaultId: id })),
      addVault: (vault) => setState((s) => ({ ...s, vaults: [...s.vaults, vault], selectedVaultId: vault.id })),
      selectFlow: (id) => setState((s) => ({ ...s, selectedFlowId: id, selectedNodeId: null })),
      setLens: (lens) => setState((s) => ({ ...s, lens })),
      selectNode: (id) => setState((s) => ({ ...s, selectedNodeId: id })),
      updateVisibleNode: (node) =>
        setState((s) => ({
          ...s,
          flows: s.flows.map((flow) => {
            if (flow.id !== s.selectedFlowId) return flow;
            const edited = flow.nodes.filter((item) => shown(item, s.lens)).map((item) => (item.id === node.id ? node : item));
            return { ...flow, nodes: mergeLensEdit(flow.nodes, s.lens, edited) };
          }),
        })),
      moveNode: (id, position) =>
        setState((s) => ({
          ...s,
          flows: s.flows.map((flow) =>
            flow.id !== s.selectedFlowId
              ? flow
              : { ...flow, nodes: flow.nodes.map((node) => (node.id === id ? { ...node, position } : node)) },
          ),
        })),
      addFromPalette: (catalogId) =>
        setState((s) => ({
          ...s,
          flows: s.flows.map((flow) => {
            if (flow.id !== s.selectedFlowId) return flow;
            const item = PALETTE.find((entry) => entry.id === catalogId);
            if (!item) return flow;
            const index = flow.nodes.reduce((max, node) => Math.max(max, node.index), -1) + 1;
            const created = nodeFromPalette(item, index);
            return { ...flow, nodes: [...flow.nodes, created] };
          }),
        })),
      setReplay: (doc) => setState((s) => ({ ...s, replay: doc, replayCursor: 0, replayPlaying: false })),
      setCursor: (cursor) => setState((s) => ({ ...s, replayCursor: cursor })),
      setPlaying: (playing) => setState((s) => ({ ...s, replayPlaying: playing })),
      setSpeed: (speed) => setState((s) => ({ ...s, replaySpeed: speed })),
      resetReplay: () => setState((s) => ({ ...s, replayCursor: 0, replayPlaying: false })),
    };
  }, [state]);

  return <Ctx.Provider value={api}>{children}</Ctx.Provider>;
}

export function useApp(): Api {
  const value = useContext(Ctx);
  if (!value) throw new Error("state missing");
  return value;
}
