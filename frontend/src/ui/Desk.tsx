import { useEffect, useState, type ReactNode } from "react";
import { Skeleton } from "skullet/react";
import { PALETTE } from "../model/draft";
import type { FlowNode } from "../model/flow";
import { loadLayouts, moveSlot, normalizeSlots, saveLayouts, toggleHidden, toggleSpan, type Slot } from "../model/layout";
import { projectLegs, type ReplayDocument, type Stage } from "../model/replay";
import { paletteGroups, type ChainPin, type ProtocolPin } from "../model/shell";

const STAGES: Stage[] = ["signed", "sent", "landed", "reported", "failed"];

function useLayout(page: string, ids: string[], wide: string[]) {
  const [all, setAll] = useState(loadLayouts);
  const slots = normalizeSlots(ids, all[page], wide);
  function commit(next: Slot[]) {
    const updated = { ...all, [page]: next };
    setAll(updated);
    saveLayouts(updated);
  }
  return {
    slots,
    move: (id: string, beforeId: string) => commit(moveSlot(slots, id, beforeId)),
    hide: (id: string) => commit(toggleHidden(slots, id)),
    span: (id: string) => commit(toggleSpan(slots, id)),
    show: (id: string) => commit(toggleHidden(slots, id)),
  };
}

function useSettled() {
  const [ready, setReady] = useState(false);
  useEffect(() => {
    const timer = window.setTimeout(() => setReady(true), 680);
    return () => window.clearTimeout(timer);
  }, []);
  return ready;
}

function shortToken(token: string): string {
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
  return token.length > 18 ? `${token.slice(0, 16)}…` : token;
}

function tokensIn(nodes: FlowNode[]): string[] {
  const found = new Set<string>();
  for (const node of nodes) {
    if (node.input !== "none") found.add(node.input);
    if (node.output) found.add(node.output);
  }
  return [...found];
}

export function PortfolioDesk({ nodes, activity }: { nodes: FlowNode[]; activity: ReplayDocument | null }) {
  const ready = useSettled();
  const layout = useLayout("portfolio", ["hero", "holdings", "activity"], ["hero"]);
  const [arrange, setArrange] = useState(false);
  const holdings = tokensIn(nodes);
  return (
    <section className="desk" aria-label="Portfolio">
      <div className="desk-head">
        <div>
          <p className="eyebrow">Portfolio</p>
          <h1>PnL</h1>
        </div>
        <button type="button" className="chip-btn" aria-pressed={arrange} onClick={() => setArrange((value) => !value)}>
          Arrange
        </button>
      </div>
      <Skeleton loading={!ready} label="Loading portfolio">
        <CardGrid
          arrange={arrange}
          slots={layout.slots}
          onMove={layout.move}
          onHide={layout.hide}
          onSpan={layout.span}
          render={(id) => {
            if (id === "hero") return <Hero />;
            if (id === "holdings") return <Holdings tokens={holdings} />;
            return <Activity doc={activity} />;
          }}
        />
      </Skeleton>
    </section>
  );
}

export function ChainDesk({
  chain,
  nodes,
  onOpen,
}: {
  chain: ChainPin;
  nodes: FlowNode[];
  onOpen: (protocolId: string) => void;
}) {
  const ready = useSettled();
  const ids = chain.protocols.map((protocol) => protocol.id);
  const layout = useLayout(`chain:${chain.id}`, ids, []);
  const [arrange, setArrange] = useState(false);
  return (
    <section className="desk" aria-label={chain.label}>
      <div className="desk-head">
        <div>
          <p className="eyebrow">{chain.label}</p>
          <h1>Protocols</h1>
        </div>
        <button type="button" className="chip-btn" aria-pressed={arrange} onClick={() => setArrange((value) => !value)}>Arrange</button>
      </div>
      <Skeleton loading={!ready} label={`Loading ${chain.label}`}>
        <CardGrid
          arrange={arrange}
          slots={layout.slots}
          onMove={layout.move}
          onHide={layout.hide}
          onSpan={layout.span}
          render={(id) => {
            const protocol = chain.protocols.find((item) => item.id === id);
            if (!protocol) return null;
            const count = nodes.filter((node) => node.chain === chain.id && node.service === protocol.id).length;
            return (
              <button type="button" className="plain" disabled={arrange} onClick={() => onOpen(protocol.id)}>
                <p className="eyebrow">{chain.label}</p>
                <h2>{protocol.label}</h2>
                <p className="quiet">{count} in this strategy</p>
                <p>Balance unknown</p>
              </button>
            );
          }}
        />
      </Skeleton>
    </section>
  );
}

export function ProtocolDesk({
  chain,
  protocol,
  nodes,
  replay,
  onAdd,
  onOpenStrategy,
}: {
  chain: ChainPin;
  protocol: ProtocolPin;
  nodes: FlowNode[];
  replay: ReplayDocument | null;
  onAdd: (catalogId: string) => void;
  onOpenStrategy: () => void;
}) {
  const ready = useSettled();
  const layout = useLayout(`protocol:${protocol.id}`, ["positions", "actions", "runs"], ["positions"]);
  const [arrange, setArrange] = useState(false);
  const mine = nodes.filter((node) => node.chain === chain.id && node.service === protocol.id);
  const actions = paletteGroups(chain.id, protocol.id).flatMap((group) => group.items);
  const stages = replay ? projectLegs(mine, replay.events) : [];
  return (
    <section className="desk" aria-label={protocol.label}>
      <div className="desk-head">
        <div>
          <p className="eyebrow">{chain.label}</p>
          <h1>{protocol.label}</h1>
        </div>
        <div className="row">
          <button type="button" className="chip-btn" onClick={onOpenStrategy}>Strategy</button>
          <button type="button" className="chip-btn" aria-pressed={arrange} onClick={() => setArrange((value) => !value)}>Arrange</button>
        </div>
      </div>
      <Skeleton loading={!ready} label={`Loading ${protocol.label}`}>
        <CardGrid
          arrange={arrange}
          slots={layout.slots}
          onMove={layout.move}
          onHide={layout.hide}
          onSpan={layout.span}
          render={(id) => {
            if (id === "positions") return <Positions nodes={mine} />;
            if (id === "actions") return <Actions protocol={protocol} items={actions} onAdd={onAdd} />;
            return <Runs stages={stages} labelled={Boolean(replay)} />;
          }}
        />
      </Skeleton>
    </section>
  );
}

function CardGrid({
  slots,
  arrange,
  onMove,
  onHide,
  onSpan,
  render,
}: {
  slots: Slot[];
  arrange: boolean;
  onMove: (id: string, beforeId: string) => void;
  onHide: (id: string) => void;
  onSpan: (id: string) => void;
  render: (id: string) => ReactNode;
}) {
  const visible = slots.filter((slot) => !slot.hidden);
  const hidden = slots.filter((slot) => slot.hidden);
  return (
    <>
      <div className="masonry">
        {visible.map((slot) => (
          <article
            key={slot.id}
            className={slot.span === 2 ? "tcard span-2" : "tcard"}
            draggable={arrange}
            onDragStart={(event) => event.dataTransfer.setData("text/plain", slot.id)}
            onDragOver={(event) => event.preventDefault()}
            onDrop={(event) => {
              event.preventDefault();
              onMove(event.dataTransfer.getData("text/plain"), slot.id);
            }}
          >
            {arrange ? (
              <div className="arrange">
                <span className="quiet">Drag</span>
                <button type="button" onClick={() => onSpan(slot.id)}>{slot.span === 1 ? "Wider" : "Narrower"}</button>
                <button type="button" onClick={() => onHide(slot.id)}>Hide</button>
              </div>
            ) : null}
            {render(slot.id)}
          </article>
        ))}
      </div>
      {hidden.length > 0 ? (
        <div className="hidden-row">
          {hidden.map((slot) => (
            <button key={slot.id} type="button" className="chip-btn" onClick={() => onHide(slot.id)}>
              Show {slot.id}
            </button>
          ))}
        </div>
      ) : null}
    </>
  );
}

function Hero() {
  return (
    <div className="hero">
      <p className="eyebrow">Total value</p>
      <p className="hero-value">Unavailable</p>
      <span className="chip neutral">unknown</span>
      <p className="quiet">Cost basis unknown</p>
    </div>
  );
}

function Holdings({ tokens }: { tokens: string[] }) {
  return (
    <div>
      <h2>Holdings</h2>
      {tokens.length === 0 ? <p>unknown</p> : (
        <ul className="feed">
          {tokens.map((token) => (
            <li key={token}>
              <strong>{shortToken(token)}</strong>
              <span>unknown</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Activity({ doc }: { doc: ReplayDocument | null }) {
  const rows = doc?.events.slice(0, 8) ?? [];
  return (
    <div>
      <h2>Activity</h2>
      {doc ? <span className="chip replay">Replay</span> : null}
      {rows.length === 0 ? <p>No activity recorded</p> : (
        <ul className="feed">
          {rows.map((event) => (
            <li key={event.id}>
              <span className="num">row {event.sourceRow}</span>
              <span className="stage-row">
                {STAGES.map((stage) => (
                  <span key={stage} className={event.evidenced.includes(stage) ? `chip ${stage === "failed" ? "bad" : "ok"}` : "chip neutral"}>
                    {stage}
                  </span>
                ))}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Positions({ nodes }: { nodes: FlowNode[] }) {
  return (
    <div>
      <h2>Positions</h2>
      {nodes.length === 0 ? <p>unknown</p> : (
        <ul className="feed">
          {nodes.map((node) => (
            <li key={node.id}>
              <div>
                <strong>{node.title}</strong>
                <div className="quiet">{node.venue}</div>
              </div>
              <span>unknown</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Actions({
  protocol,
  items,
  onAdd,
}: {
  protocol: ProtocolPin;
  items: (typeof PALETTE)[number][];
  onAdd: (catalogId: string) => void;
}) {
  return (
    <div>
      <h2>Actions</h2>
      {!protocol.flowNode || items.length === 0 ? <p>Owner-signed swap. Not a flow node.</p> : (
        <ul className="feed">
          {items.map((item) => (
            <li key={item.id}>
              <div>
                <strong>{item.title}</strong>
                <div className="quiet">{item.serializable ? item.venue : "Local"}</div>
              </div>
              <button type="button" className="chip-btn" onClick={() => onAdd(item.id)}>Add</button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

function Runs({ stages, labelled }: { stages: ReturnType<typeof projectLegs>; labelled: boolean }) {
  return (
    <div>
      <h2>Runs</h2>
      {labelled ? <span className="chip replay">Replay</span> : null}
      {stages.length === 0 ? <p>No runs recorded</p> : (
        <ul className="feed">
          {stages.map((stage) => (
            <li key={stage.nodeId}>
              <span className="num">leg {stage.leg}</span>
              <span className="stage-row">
                {stage.evidenced.map((name) => <span key={name} className="chip ok">{name}</span>)}
                {stage.notRecorded.map((name) => <span key={name} className="chip neutral">{name}</span>)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
