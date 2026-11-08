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
    <article className={selected ? "step selected" : "step"}>
      <Handle type="target" position={Position.Left} />
      <header>
        <strong>{data.node.title}</strong>
        <span className="muted">{data.node.chain}</span>
      </header>
      <p className="venue">{data.node.venue}</p>
      <div className="chips">
        <span className="chip">{shortToken(data.node.input)}</span>
        <span className="chip">{data.node.output ? shortToken(data.node.output) : "no output"}</span>
        {left != null && left > 0 ? <span className="chip">{left} bps free</span> : null}
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
  onSelect,
  onDetails,
  onMove,
  onConnectEnds,
}: {
  nodes: FlowNode[];
  stages: NodeStage[];
  selectedId: string | null;
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
          style: { stroke: "#71717a", strokeWidth: node.id === selectedId || target.id === selectedId ? 2 : 1.5 },
          labelStyle: { fill: "#fafafa", fontSize: 12 },
          labelBgStyle: { fill: "#18181b" },
          labelBgPadding: [6, 3] as [number, number],
          labelBgBorderRadius: 8,
        }];
      }),
    );
    return stored;
  }, [nodes, selectedId]);

  const selected = nodes.find((node) => node.id === selectedId);
  const defaultViewport = selected
    ? { x: 120 - selected.position.x, y: 80 - selected.position.y, zoom: 1 }
    : { x: 48, y: 48, zoom: 1 };

  return (
    <div className="canvas-wrap">
      <div className="canvas-tools">
        <button type="button" className="btn" onClick={() => flow.fitView({ padding: 0.15, duration: 180, maxZoom: 1 })}>
          Fit full board
        </button>
        <button
          type="button"
          className="btn"
          disabled={!selected}
          onClick={() => selected && flow.fitView({ nodes: [{ id: selected.id }], padding: 0.4, duration: 180, maxZoom: 1 })}
        >
          Focus selected
        </button>
      </div>
      <ReactFlow
        nodes={flowNodes}
        edges={edges}
        nodeTypes={nodeTypes}
        defaultViewport={defaultViewport}
        minZoom={0.4}
        maxZoom={1.5}
        proOptions={{ hideAttribution: true }}
        onNodeClick={(_, node) => onSelect(node.id)}
        onNodeDragStop={(_, node) => onMove(node.id, node.position)}
        onConnect={(connection) => {
          if (connection.source && connection.target) onConnectEnds(connection.source, connection.target);
        }}
        nodesDraggable
        elementsSelectable
        panOnScroll
      >
        <Background color="#27272a" gap={24} size={1} />
        <Controls showInteractive={false} />
      </ReactFlow>
    </div>
  );
}

export function FlowBoard(props: {
  nodes: FlowNode[];
  stages: NodeStage[];
  selectedId: string | null;
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
