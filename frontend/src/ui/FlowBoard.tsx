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
import { edgeCaption } from "../model/compose";
import type { FlowNode } from "../model/flow";
import { DEMO_ACCOUNT } from "../demo/fixtures";
import { nodeReadout, type LiveBook } from "../live/quotes";
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
  const quote = nodeReadout({
    demo: data.demo,
    token: data.node.output ?? data.node.input,
    planAmount: data.node.planAmount,
    demoAmount: data.demo ? "420 NEAR" : undefined,
    demoEntry: data.demo ? DEMO_ACCOUNT.nearPrice.toFixed(2) : undefined,
    book: data.book,
    now: data.now,
  });
  const running = data.stages.includes("signed") && !data.stages.includes("reported") && !data.stages.includes("failed");
  const status = data.demo ? "demo" : running ? "running" : "idle";
  return (
    <article className={selected ? "step selected" : "step"} data-chain={data.node.chain} data-pulse={data.pulse ? "1" : "0"}>
      <Handle type="target" position={Position.Left} />
      <header>
        <strong>{data.node.title}</strong>
        <span className={`status ${status}`}>{status}</span>
      </header>
      <p className="venue">{data.node.venue} · {chainLabel(data.node.chain)}{data.node.auto ? " · auto" : ""}</p>
      <dl className="quote">
        <div><dt>Entry {quote.entryTag ? <span className="chip demo-tag">{quote.entryTag}</span> : null}</dt><dd>{quote.entry}</dd></div>
        <div><dt>Amount {quote.amountTag ? <span className="chip demo-tag">{quote.amountTag}</span> : null}</dt><dd>{quote.amount}</dd></div>
        <div><dt>Value {quote.currentTag ? <span className="chip ok">{quote.currentTag}</span> : null}</dt><dd className={data.pulse ? "pulse" : ""}>{quote.current}</dd></div>
        <div><dt>Change</dt><dd className={quote.changeUp == null ? "" : quote.changeUp ? "up" : "down"}>{quote.change ?? "unknown"}</dd></div>
      </dl>
      {quote.spark.length > 1 ? <Spark points={quote.spark} up={quote.changeUp !== false} /> : null}
      <p className="ago">{quote.ago}</p>
      <div className="chips">
        {STAGE_NAMES.map((stage) => (
          <span key={stage} className={data.stages.includes(stage) ? `chip ${stage === "failed" ? "bad" : "ok"}` : "chip neutral"}>{stage}</span>
        ))}
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
  const flowNodes = useMemo<Node<Data>[]>(() => {
    return nodes.map((node) => ({
      id: node.id,
      type: "step",
      position: { x: node.position.x, y: node.position.y },
      data: { node, stages: stageById.get(node.id) ?? [], demo, book, now, pulse, onOpen: onDetails },
      selected: node.id === selectedId,
    }));
  }, [nodes, stageById, selectedId, onDetails, demo, book, now, pulse]);

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
