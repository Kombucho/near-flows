import { useMemo } from "react";
import {
  Background,
  Controls,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useViewport,
  type Edge,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { edgeCaption } from "../model/compose";
import type { FlowNode } from "../model/flow";
import { layoutGraph } from "../model/place";
import { DEMO_ACCOUNT } from "../demo/fixtures";
import { nodeReadout, quotedToken, type LiveBook } from "../live/quotes";
import { chainLabel } from "../model/shell";
import type { NodeStage, Stage } from "../model/replay";
import { Spark } from "./bits";

type Data = {
  node: FlowNode;
  stages: Stage[];
  demo: boolean;
  book: LiveBook;
  now: number;
  pulse: boolean;
  onOpen: (id: string) => void;
};

const STAGE_NAMES: Stage[] = ["signed", "sent", "landed", "reported", "failed"];

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
  const { zoom } = useViewport();
  const readout = nodeReadout({
    demo: data.demo,
    token: quotedToken(data.node.input, data.node.output),
    service: data.node.service,
    inputToken: data.node.input,
    planAmount: data.node.planAmount,
    demoEntry: data.demo ? DEMO_ACCOUNT.nearPrice.toFixed(2) : undefined,
    book: data.book,
    now: data.now,
  });
  const running = data.stages.includes("signed") && !data.stages.includes("reported") && !data.stages.includes("failed");
  const status = data.demo ? "demo" : running ? "running" : "idle";
  const failed = data.stages.includes("failed");
  if (zoom < 0.72) {
    return (
      <article className={selected ? "step step-mini selected" : "step step-mini"} data-chain={data.node.chain}>
        <Handle type="target" position={Position.Left} />
        <header>
          <strong>{data.node.title}</strong>
          <span className={readout.changeUp == null ? "muted" : readout.changeUp ? "up" : "down"}>{readout.change ?? "unknown"}</span>
        </header>
        <p className="mini-value num">{readout.current}</p>
        <Handle type="source" position={Position.Right} />
      </article>
    );
  }
  return (
    <article className={selected ? "step selected" : "step"} data-chain={data.node.chain} data-pulse={data.pulse ? "1" : "0"}>
      <Handle type="target" position={Position.Left} />
      <header>
        <strong>{data.node.title}</strong>
        <span className={`status ${status}`}>{status}</span>
      </header>
      <p className="venue">{data.node.venue} · {chainLabel(data.node.chain)}{data.node.auto ? " · auto" : ""}</p>
      <div className="quote">
        <div>
          <div className="q-label">Entry {readout.entryTag ? <span className="chip demo-tag">{readout.entryTag}</span> : null}</div>
          <div className="q-value num">{readout.entry}</div>
        </div>
        <div>
          <div className="q-label">Amount {readout.amountTag ? <span className="chip demo-tag">{readout.amountTag}</span> : null}</div>
          <div className="q-value num">{readout.amount}</div>
        </div>
        <div>
          <div className="q-label">Value {readout.currentTag ? <span className="chip ok">{readout.currentTag}</span> : null}</div>
          <div className={data.pulse ? "q-value num pulse" : "q-value num"}>{readout.current}</div>
        </div>
        <div className="q-change">
          <div className="q-label">Change</div>
          <div className={readout.changeUp == null ? "q-value" : readout.changeUp ? "q-value up" : "q-value down"}>{readout.change ?? "unknown"}</div>
        </div>
      </div>
      {readout.spark.length > 1 ? <Spark points={readout.spark} up={readout.changeUp !== false} /> : null}
      <p className="ago">{readout.ago}</p>
      <div className="chips">
        {STAGE_NAMES.filter((stage) => stage !== "failed").map((stage) => (
          <span key={stage} className={data.stages.includes(stage) ? "chip ok" : "chip wait"}>{stage}</span>
        ))}
        {failed ? <span className="chip bad">failed</span> : null}
      </div>
      <footer>
        <span className="muted">{shortToken(data.node.input)} → {data.node.output ? shortToken(data.node.output) : "held"}</span>
        <button type="button" className="btn-quiet nodrag nopan" onClick={() => data.onOpen(data.node.id)}>Open</button>
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
  demo,
  book,
  now,
  pulse,
  onSelect,
  onDetails,
  onMove,
  onConnectEnds,
  onCompose,
  onComposeFrom,
}: {
  nodes: FlowNode[];
  stages: NodeStage[];
  selectedId: string | null;
  theme: "light" | "dark";
  focusKey: string;
  demo: boolean;
  book: LiveBook;
  now: number;
  pulse: boolean;
  onSelect: (id: string) => void;
  onDetails: (id: string) => void;
  onMove: (id: string, position: { x: number; y: number }) => void;
  onConnectEnds: (sourceId: string, targetId: string) => void;
  onCompose: () => void;
  onComposeFrom: (sourceId: string) => void;
}) {
  const flow = useReactFlow();
  const stageById = useMemo(() => new Map(stages.map((stage) => [stage.nodeId, stage.evidenced])), [stages]);
  const placed = useMemo(() => new Map(layoutGraph(nodes).map((item) => [item.id, item])), [nodes]);
  const flowNodes = useMemo<Node<Data>[]>(() => {
    return nodes.map((node) => ({
      id: node.id,
      type: "step",
      position: placed.get(node.id) ?? node.position,
      data: { node, stages: stageById.get(node.id) ?? [], demo, book, now, pulse, onOpen: onDetails },
      selected: node.id === selectedId,
    }));
  }, [nodes, placed, stageById, selectedId, onDetails, demo, book, now, pulse]);

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
          label: edgeCaption(node, route.bps),
          className: "flow-edge",
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
    const at = placed.get(node.id) ?? node.position;
    if (!best || at.x < best.x || (at.x === best.x && at.y < best.y)) return at;
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
        onNodeClick={(_, node) => { onSelect(node.id); onDetails(node.id); }}
        onPaneClick={() => { onSelect(""); onCompose(); }}
        onNodeDragStop={(_, node) => onMove(node.id, node.position)}
        onConnect={(connection) => {
          if (connection.source && connection.target) onConnectEnds(connection.source, connection.target);
        }}
        onConnectEnd={(_, state) => {
          if (!state.toNode && state.fromNode) onComposeFrom(state.fromNode.id);
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
  demo: boolean;
  book: LiveBook;
  now: number;
  pulse: boolean;
  onSelect: (id: string) => void;
  onDetails: (id: string) => void;
  onMove: (id: string, position: { x: number; y: number }) => void;
  onConnectEnds: (sourceId: string, targetId: string) => void;
  onCompose: () => void;
  onComposeFrom: (sourceId: string) => void;
}) {
  return (
    <ReactFlowProvider>
      <BoardInner {...props} />
    </ReactFlowProvider>
  );
}
