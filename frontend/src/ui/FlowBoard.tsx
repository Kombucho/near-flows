import { useMemo } from "react";
import {
  Background,
  Controls,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import type { FlowNode } from "../model/flow";
import { remainderBps } from "../model/flow";
import { chainLabel } from "../model/shell";
import type { NodeStage, Stage } from "../model/replay";

type Data = {
  node: FlowNode;
  stages: Stage[];
  onOpen: (id: string) => void;
};

function stageLabel(stages: Stage[]): string {
  if (stages.length === 0) return "Awaiting evidence";
  return stages.join(" · ");
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

function StepNode({ data, selected }: NodeProps<Node<Data>>) {
  const stages = data.stages;
  const latest = stages.at(-1);
  const left = remainderBps(data.node);
  return (
    <article className={selected ? "step selected" : "step"} data-chain={data.node.chain}>
      <Handle type="target" position={Position.Left} />
      <header>
        <strong>{data.node.title}</strong>
        <span className="muted">{chainLabel(data.node.chain)}</span>
      </header>
      <p className="venue">{data.node.venue}</p>
      <div className="chips">
        <span className="chip">{shortToken(data.node.input)}</span>
        <span className="chip">{data.node.output ? shortToken(data.node.output) : "no output"}</span>
        {left != null && left > 0 ? <span className="chip">{left} bps free</span> : null}
        {!data.node.serializable ? <span className="chip">Local</span> : null}
      </div>
      <footer>
        <span className={latest ? `stage ${latest}` : "stage"}>{stages.length ? stageLabel(stages) : "Not recorded"}</span>
        <button type="button" className="btn-quiet nodrag nopan" onClick={() => data.onOpen(data.node.id)}>
          Details
        </button>
      </footer>
      <Handle type="source" position={Position.Right} />
    </article>
  );
}

const nodeTypes = { step: StepNode };

function BoardInner({
  nodes,
  stages,
  selectedId,
  theme,
  focusKey,
  onSelect,
  onDetails,
  onMove,
  onConnectEnds,
}: {
  nodes: FlowNode[];
  stages: NodeStage[];
  selectedId: string | null;
  theme: "light" | "dark";
  focusKey: string;
  onSelect: (id: string) => void;
  onDetails: (id: string) => void;
  onMove: (id: string, position: { x: number; y: number }) => void;
  onConnectEnds: (sourceId: string, targetId: string) => void;
}) {
  const flow = useReactFlow();
  const stageById = useMemo(() => new Map(stages.map((stage) => [stage.nodeId, stage.evidenced])), [stages]);
  const flowNodes = useMemo<Node<Data>[]>(() => {
    return nodes.map((node) => ({
      id: node.id,
      type: "step",
      position: { x: node.position.x, y: node.position.y },
      data: { node, stages: stageById.get(node.id) ?? [], onOpen: onDetails },
      selected: node.id === selectedId,
    }));
  }, [nodes, stageById, selectedId, onDetails]);

  const edges = useMemo<Edge[]>(() => {
    const byIndex = new Map(nodes.map((node) => [node.index, node]));
    const stored: Edge[] = nodes.flatMap((node) =>
      node.routes.flatMap((route) => {
        const target = byIndex.get(route.to);
        if (!target) return [];
        return [{
          id: `${node.id}-${target.id}-${route.bps}`,
          source: node.id,
          target: target.id,
          label: `${(route.bps / 100).toFixed(2).replace(/\.00$/, "")}%`,
          style: { stroke: theme === "light" ? "#a8a29e" : "#78716c", strokeWidth: node.id === selectedId || target.id === selectedId ? 2 : 1.5 },
          labelStyle: { fill: theme === "light" ? "#1c1917" : "#f5f4f1", fontSize: 12, fontWeight: 560 },
          labelBgStyle: { fill: theme === "light" ? "#fffcf8" : "#1c1b19" },
          labelBgPadding: [6, 3] as [number, number],
          labelBgBorderRadius: 8,
        }];
      }),
    );
    return stored;
  }, [nodes, selectedId, theme]);

  const anchor = nodes.reduce<{ x: number; y: number } | null>((best, node) => {
    if (!best || node.position.x < best.x || (node.position.x === best.x && node.position.y < best.y)) return node.position;
    return best;
  }, null);
  const narrow = typeof window !== "undefined" && window.matchMedia("(max-width: 860px)").matches;
  const top = narrow ? 288 : 176;
  const left = narrow ? 16 : 80;
  const defaultViewport = anchor ? { x: left - anchor.x, y: top - anchor.y, zoom: 1 } : { x: left, y: top, zoom: 1 };
  const selected = nodes.find((node) => node.id === selectedId);

  return (
    <div className="canvas-wrap">
      <div className="canvas-tools">
        <button type="button" onClick={() => flow.fitView({ padding: 0.28, duration: 180, maxZoom: 1 })}>
          Fit
        </button>
        <button
          type="button"
          disabled={!selected}
          onClick={() => selected && flow.fitView({ nodes: [{ id: selected.id }], padding: 0.45, duration: 180, maxZoom: 1 })}
        >
          Focus
        </button>
      </div>
      <ReactFlow
        key={focusKey}
        nodes={flowNodes}
        edges={edges}
        nodeTypes={nodeTypes}
        colorMode={theme}
        defaultViewport={defaultViewport}
        minZoom={0.45}
        maxZoom={1.6}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_, node) => onSelect(node.id)}
        onPaneClick={() => onSelect("")}
        onNodeDragStop={(_, node) => onMove(node.id, node.position)}
        onConnect={(connection) => {
          if (connection.source && connection.target) onConnectEnds(connection.source, connection.target);
        }}
        nodesDraggable
        elementsSelectable
        panOnScroll
        fitView={false}
      >
        <Background color={theme === "light" ? "#d6d1c8" : "#2a2926"} gap={28} size={1.2} />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}

export function FlowBoard(props: {
  nodes: FlowNode[];
  stages: NodeStage[];
  selectedId: string | null;
  theme: "light" | "dark";
  focusKey: string;
  onSelect: (id: string) => void;
  onDetails: (id: string) => void;
  onMove: (id: string, position: { x: number; y: number }) => void;
  onConnectEnds: (sourceId: string, targetId: string) => void;
}) {
  return (
    <ReactFlowProvider>
      <BoardInner {...props} />
    </ReactFlowProvider>
  );
}
