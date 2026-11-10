import { useEffect, useMemo } from "react";
import {
  Background,
  BaseEdge,
  Controls,
  EdgeLabelRenderer,
  getSmoothStepPath,
  Handle,
  Position,
  ReactFlow,
  ReactFlowProvider,
  useReactFlow,
  useUpdateNodeInternals,
  useViewport,
  type Edge,
  type EdgeProps,
  type Node,
  type NodeProps,
} from "@xyflow/react";
import "@xyflow/react/dist/style.css";
import { edgeCaption } from "../model/compose";
import type { FlowNode } from "../model/flow";
import { layoutGraph } from "../model/place";
import type { LiveBook } from "../live/quotes";
import type { NodeStage, Stage } from "../model/replay";
import { positionOwner } from "./cardModel";
import { ProtocolCard } from "./ProtocolCard";

type Data = {
  node: FlowNode;
  stages: Stage[];
  demo: boolean;
  book: LiveBook;
  now: number;
  pulse: boolean;
  usePosition: boolean;
  onOpen: (id: string) => void;
};

function StepNode({ id, data, selected, dragging }: NodeProps<Node<Data>>) {
  const { zoom } = useViewport();
  const compact = zoom < 0.6;
  const update = useUpdateNodeInternals();
  useEffect(() => {
    update(id);
  }, [compact, id, update]);
  return (
    <div className={selected ? "pcard-shell selected" : "pcard-shell"}>
      <Handle type="target" position={Position.Left} />
      <ProtocolCard
        node={data.node}
        demo={data.demo}
        book={data.book}
        now={data.now}
        evidenced={data.stages}
        usePosition={data.usePosition}
        compact={compact}
        dragging={Boolean(dragging)}
        pulse={data.pulse}
        selected={Boolean(selected)}
        onOpen={data.onOpen}
      />
      <Handle type="source" position={Position.Right} />
    </div>
  );
}

function LtrEdge({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  style,
  markerEnd,
  label,
  labelStyle,
  labelBgStyle,
  labelBgPadding,
  labelBgBorderRadius,
}: EdgeProps) {
  const [path, labelX, labelY] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
    borderRadius: 12,
    offset: 28,
  });
  const padding = labelBgPadding ?? [6, 3];
  return (
    <>
      <BaseEdge id={id} path={path} style={style} markerEnd={markerEnd} interactionWidth={18} />
      {label ? (
        <EdgeLabelRenderer>
          <div
            className="nodrag nopan edge-label"
            style={{
              position: "absolute",
              transform: `translate(-50%, -50%) translate(${labelX}px, ${labelY}px)`,
              background: labelBgStyle?.fill as string | undefined,
              color: (labelStyle?.fill as string | undefined) ?? undefined,
              fontSize: labelStyle?.fontSize,
              fontWeight: labelStyle?.fontWeight,
              padding: `${padding[1]}px ${padding[0]}px`,
              borderRadius: labelBgBorderRadius,
            }}
          >
            {label}
          </div>
        </EdgeLabelRenderer>
      ) : null}
    </>
  );
}

const nodeTypes = { step: StepNode };
const edgeTypes = { ltr: LtrEdge };

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
  const owners = useMemo(() => positionOwner(nodes), [nodes]);
  const flowNodes = useMemo<Node<Data>[]>(() => {
    return nodes.map((node) => ({
      id: node.id,
      type: "step",
      position: placed.get(node.id) ?? node.position,
      data: { node, stages: stageById.get(node.id) ?? [], demo, book, now, pulse, usePosition: owners.has(node.id), onOpen: onDetails },
      selected: node.id === selectedId,
    }));
  }, [nodes, placed, stageById, selectedId, onDetails, demo, book, now, pulse, owners]);

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
          type: "ltr",
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
        edgeTypes={edgeTypes}
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
        <Background color={theme === "light" ? "#d3cec2" : "#1c1c1f"} gap={22} size={1} />
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
