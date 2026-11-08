import { useEffect, useId, useMemo, useState } from "react";
import { NavLink, Route, Routes, useNavigate } from "react-router-dom";
import { ABI } from "./model/abi";
import { PALETTE } from "./model/draft";
import { contractSerialize, exportLocalDraft, importLocalDraft, proposeRoute, remainderBps, validateFlow, visibleNodes, type ChainId, type FlowNode } from "./model/flow";
import { LIMITS, SOURCE_COMMIT } from "./model/limits";
import { parseDemoLog, projectLegs, type ReplayDocument, type Stage } from "./model/replay";
import { useApp, type VaultDraft } from "./state";
import { FlowBoard } from "./ui/FlowBoard";

function Shell({ children }: { children: React.ReactNode }) {
  return (
    <div className="app">
      <nav className="nav" aria-label="Primary">
        <div className="brand">
          <div className="mark" aria-hidden="true">流</div>
          <div>
            <strong>NEAR Flows</strong>
            <p>Draft workspace</p>
          </div>
        </div>
        <NavLink to="/" end>Overview</NavLink>
        <NavLink to="/vaults">Vaults</NavLink>
        <NavLink to="/flows">Flows</NavLink>
        <NavLink to="/demo">Demo</NavLink>
        <NavLink to="/settings">Settings</NavLink>
        <NavLink to="/feed">Public feed</NavLink>
      </nav>
      <div className="main">{children}</div>
    </div>
  );
}

function ModeBadge() {
  const app = useApp();
  const mode = app.replay ? "RECORDED REPLAY" : "DRAFT";
  return (
    <span className="badge">
      <strong>{mode}</strong>
      {app.replay ? " · imported DEMO| log" : " · nothing is sent"}
    </span>
  );
}

function Overview() {
  const app = useApp();
  const flow = app.flows.find((item) => item.id === app.selectedFlowId);
  const issues = flow ? validateFlow(flow.nodes) : [];
  return (
    <Shell>
      <header className="topbar">
        <ModeBadge />
        <span className="badge">testnet concept · opt-in · disarmed</span>
        <span className="spacer" />
        <span className="muted num">source {SOURCE_COMMIT.slice(0, 7)}</span>
      </header>
      <div className="page">
        <p className="kicker">Overview</p>
        <h1 style={{ margin: 0, fontSize: 28, fontWeight: 560 }}>Where a draft sits. Not what it earned.</h1>
        <div className="grid">
          <article className="card">
            <p className="kicker">Local vault drafts</p>
            <div className="stat num">{app.vaults.length}</div>
            <p>Not chain accounts. Plus stores another local draft after review.</p>
          </article>
          <article className="card">
            <p className="kicker">Attention in this draft</p>
            <div className="stat num">{issues.length}</div>
            <p>Validation against this commit’s route rules. Not a failed transaction.</p>
          </article>
          <article className="card">
            <p className="kicker">Cost basis / PnL</p>
            <div className="stat">Unknown</div>
            <p>No cost basis is stored. Missing profit is not zero.</p>
          </article>
          <article className="card">
            <p className="kicker">Keeper</p>
            <div className="stat">No heartbeat</div>
            <p>Last observed step time is not uptime. Nothing has been observed.</p>
          </article>
        </div>
        <article className="card">
          <h2>Source mismatch</h2>
          <p>
            origin/main is still {SOURCE_COMMIT}. User-reported ABI changes are not in that commit.
            The workspace does not invent the missing methods.
          </p>
        </article>
      </div>
    </Shell>
  );
}

function Vaults() {
  const app = useApp();
  const [open, setOpen] = useState(false);
  return (
    <Shell>
      <header className="topbar">
        <ModeBadge />
        <span className="spacer" />
        <button type="button" className="btn-primary" onClick={() => setOpen(true)}>Plus vault</button>
      </header>
      <div className="page">
        <p className="kicker">Login {app.loginLabel}</p>
        <h1 style={{ margin: 0, fontSize: 24 }}>Vault drafts</h1>
        <ul className="list">
          {app.vaults.map((vault) => (
            <li key={vault.id}>
              <button type="button" className="row-btn" onClick={() => app.selectVault(vault.id)}>
                <strong>{vault.label}</strong>
                <div className="muted">{vault.kind === "stealth" ? "Stealth key owner. Not anonymity." : "Named draft. Not a chain account."}</div>
                {app.selectedVaultId === vault.id ? <div>Selected</div> : null}
              </button>
            </li>
          ))}
        </ul>
        <article className="card">
          <h2>On this commit</h2>
          <p>
            factory_relay.create_vault still returns “you already have a vault” for a second named vault
            from the same signer. create_stealth_vault is separate and does not record the payer.
            That on-chain limit is not applied to these local drafts, and Plus does not send either call.
          </p>
        </article>
      </div>
      {open ? <CreateVault onClose={() => setOpen(false)} /> : null}
    </Shell>
  );
}

function CreateVault({ onClose }: { onClose: () => void }) {
  const app = useApp();
  const titleId = useId();
  const [kind, setKind] = useState<VaultDraft["kind"]>("named");
  const [label, setLabel] = useState("");
  const [step, setStep] = useState<"edit" | "review">("edit");
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="dialog-back" role="presentation">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <p className="kicker">Create vault · review only</p>
        <h2 id={titleId} style={{ margin: 0 }}>No transaction is built</h2>
        <p className="muted">
          User-reported create arguments are not in {SOURCE_COMMIT.slice(0, 7)}. This form does not serialize
          create_vault. A confirmed result is only another local draft.
        </p>
        {step === "edit" ? (
          <>
            <label>Label <input value={label} onChange={(event) => setLabel(event.target.value)} /></label>
            <label>
              Kind
              <select value={kind} onChange={(event) => setKind(event.target.value as VaultDraft["kind"])}>
                <option value="named">Named</option>
                <option value="stealth">Stealth</option>
              </select>
            </label>
            <p className="muted">Keeper price and factory id are unknown. Paid provider price is unknown. Own-keeper software is free; hosting cost is not shown as zero.</p>
            <div style={{ display: "flex", gap: 8 }}>
              <button type="button" className="btn" onClick={onClose}>Cancel</button>
              <button type="button" className="btn-primary" disabled={label.trim().length < 2} onClick={() => setStep("review")}>Review</button>
            </div>
          </>
        ) : (
          <>
            <dl>
              <dt>Network</dt><dd>Testnet concept. No RPC selected.</dd>
              <dt>Kind</dt><dd>{kind}</dd>
              <dt>Label</dt><dd>{label}</dd>
              <dt>Deposit</dt><dd>Not asserted. This commit’s factory text says 0.1 NEAR minimum. User reports that figure is stale.</dd>
              <dt>Submit</dt><dd>Disarmed.</dd>
            </dl>
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button type="button" className="btn" onClick={() => setStep("edit")}>Back</button>
              <button type="button" className="btn-primary" disabled>Submit testnet request</button>
              <button
                type="button"
                className="btn"
                onClick={() => {
                  app.addVault({ id: `local-${Date.now()}`, label: label.trim(), kind, loginLabel: app.loginLabel });
                  onClose();
                }}
              >
                Keep as local draft
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}

function Flows() {
  const app = useApp();
  const navigate = useNavigate();
  const flow = app.flows.find((item) => item.vaultId === app.selectedVaultId) ?? app.flows[0];
  return (
    <Shell>
      <header className="topbar">
        <ModeBadge />
        <span className="spacer" />
      </header>
      <div className="page">
        <h1 style={{ margin: 0, fontSize: 24 }}>Flows</h1>
        <ul className="list">
          {app.flows.filter((item) => item.vaultId === app.selectedVaultId || app.flows.length === 1).map((item) => (
            <li key={item.id}>
              <button type="button" className="row-btn" onClick={() => { app.selectFlow(item.id); navigate("/flows/" + item.id); }}>
                <strong>{item.name}</strong>
                <div className="muted">{item.nodes.length} nodes in the full document</div>
              </button>
            </li>
          ))}
        </ul>
        {flow ? <p className="muted">Open keeps every node. A chain filter does not delete the others.</p> : null}
      </div>
    </Shell>
  );
}

function FlowWorkspace() {
  const app = useApp();
  const flow = app.flows.find((item) => item.id === app.selectedFlowId) ?? app.flows[0];
  const [detail, setDetail] = useState(false);
  const [review, setReview] = useState(false);
  const [listMode, setListMode] = useState(false);
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [inspectorOpen, setInspectorOpen] = useState(true);
  const [routeNote, setRouteNote] = useState<string | null>(null);
  const [draftNote, setDraftNote] = useState<string | null>(null);
  const visible = useMemo(() => visibleNodes(flow.nodes, app.lens), [flow.nodes, app.lens]);
  const issues = useMemo(() => validateFlow(flow.nodes), [flow.nodes]);
  const revealed = app.replay ? app.replay.events.slice(0, app.replayCursor) : [];
  const stages = app.replay ? projectLegs(flow.nodes, revealed) : [];
  const selected = flow.nodes.find((node) => node.id === app.selectedNodeId) ?? null;
  const services = [...new Set(flow.nodes.map((node) => node.service))];

  return (
    <Shell>
      <header className="topbar">
        <ModeBadge />
        <span className="badge">{flow.name}</span>
        <span className="muted num">{visible.length} visible / {flow.nodes.length} total</span>
        <span className="spacer" />
        <button type="button" className="btn" onClick={() => setPaletteOpen((value) => !value)}>{paletteOpen ? "Hide palette" : "Show palette"}</button>
        <button type="button" className="btn" onClick={() => setInspectorOpen((value) => !value)}>{inspectorOpen ? "Hide inspector" : "Show inspector"}</button>
        <button type="button" className="btn" onClick={() => setListMode((value) => !value)}>{listMode ? "Graph" : "List"}</button>
        <button
          type="button"
          className="btn"
          onClick={() => {
            const text = exportLocalDraft(flow);
            localStorage.setItem("near-flows.local-draft-file", text);
            const hidden = flow.nodes.length - visible.length;
            const uiOnly = flow.nodes.filter((node) => !node.serializable).map((node) => node.title);
            setDraftNote(`Local draft saved. ${flow.nodes.length} nodes kept. Hidden by the lens: ${hidden}. UI-only: ${uiOnly.join(", ") || "none"}.`);
          }}
        >
          Save local draft
        </button>
        <label className="btn">
          Import local draft
          <input
            aria-label="Import local draft"
            type="file"
            accept="application/json,.json"
            hidden
            onChange={(event) => {
              const file = event.target.files?.[0];
              if (!file) return;
              file.text().then((text) => {
                try {
                  const restored = importLocalDraft(text);
                  app.replaceNodes(restored.nodes);
                  setDraftNote(`Imported ${restored.nodes.length} nodes, including UI-only nodes.`);
                } catch {
                  setDraftNote("Import refused. The file is not a local draft.");
                }
              });
            }}
          />
        </label>
        <button type="button" className="btn-primary" onClick={() => setReview(true)}>Review</button>
      </header>
      <div className="toolbar">
        <label>
          Chain
          <select
            aria-label="Chain filter"
            value={app.lens.chain}
            onChange={(event) => app.setLens({ ...app.lens, chain: event.target.value as ChainId | "all" })}
          >
            <option value="all">All chains</option>
            <option value="near">NEAR</option>
            <option value="solana">Solana</option>
            <option value="robinhood">Robinhood Chain</option>
          </select>
        </label>
        <label>
          Service
          <select aria-label="Service filter" value={app.lens.service} onChange={(event) => app.setLens({ ...app.lens, service: event.target.value })}>
            <option value="all">All services</option>
            {services.map((service) => <option key={service} value={service}>{service}</option>)}
          </select>
        </label>
        <span className="muted">A lens hides nodes on screen. Save local draft keeps every node, including the Intents hop.</span>
        {draftNote ? <span className="muted">{draftNote}</span> : null}
        {routeNote ? <span role="status">{routeNote}</span> : null}
        <span className="badge">Public / private <strong>read-only</strong></span>
      </div>
      <div className={`workspace${paletteOpen ? "" : " no-palette"}${inspectorOpen ? "" : " no-inspector"}`}>
        {paletteOpen ? <aside className="palette">
          <p className="kicker">Palette</p>
          {PALETTE.map((item) => (
            <button key={item.id} type="button" className="item" onClick={() => app.addFromPalette(item.id)}>
              <strong>{item.title}</strong>
              <div className="muted">{item.serializable ? item.venue : "Stays in the local draft. Blocks a contract payload."}</div>
            </button>
          ))}
        </aside> : null}
        {listMode ? (
          <ol className="steps" style={{ display: "grid" }}>
            {visible.map((node) => (
              <li key={node.id}><button type="button" className="row-btn" onClick={() => app.selectNode(node.id)}>{node.index}. {node.title}</button></li>
            ))}
          </ol>
        ) : (
          <FlowBoard
            nodes={visible}
            stages={stages}
            selectedId={app.selectedNodeId}
            onSelect={(id) => app.selectNode(id)}
            onDetails={(id) => { app.selectNode(id); setDetail(true); }}
            onMove={app.moveNode}
            onConnectEnds={(sourceId, targetId) => {
              const result = proposeRoute(flow.nodes, sourceId, targetId, 1000);
              if (!result.ok) setRouteNote(`${result.code}: ${result.message}`);
              else {
                app.replaceNodes(result.nodes);
                setRouteNote("Route stored at 1000 bps. Unallocated bps stay free balance.");
              }
            }}
          />
        )}
        {inspectorOpen ? <Inspector node={selected} nodes={flow.nodes} issues={issues.filter((issue) => !selected || issue.nodeId === selected.id || !issue.nodeId)} onChange={app.updateVisibleNode} onRouteNote={setRouteNote} /> : null}
      </div>
      {detail && selected ? <Detail node={selected} onClose={() => setDetail(false)} /> : null}
      {review ? <Review flowName={flow.name} nodes={flow.nodes} onClose={() => setReview(false)} /> : null}
    </Shell>
  );
}

function Inspector({
  node,
  nodes,
  issues,
  onChange,
  onRouteNote,
}: {
  node: FlowNode | null;
  nodes: FlowNode[];
  issues: { code: string; message: string }[];
  onChange: (node: FlowNode) => void;
  onRouteNote: (note: string) => void;
}) {
  if (!node) {
    return <aside className="inspector"><p className="kicker">Inspector</p><p className="muted">Select a node. Nothing is live.</p></aside>;
  }
  const left = remainderBps(node);
  return (
    <aside className="inspector">
      <p className="kicker">Inspector</p>
      <h2 style={{ marginTop: 0 }}>{node.title}</h2>
      <dl>
        <dt>Venue</dt><dd>{node.venue}</dd>
        <dt>Input</dt><dd className="num">{node.input}</dd>
        <dt>Output</dt><dd className="num">{node.output ?? "none"}</dd>
        <dt>Trigger</dt><dd>{node.trigger.when}</dd>
        <dt>Routes</dt><dd className="num">{node.routes.length} / {LIMITS.maxRoutes}</dd>
        <dt>Free balance</dt><dd className="num">{left == null ? "No output" : `${left} bps unallocated`}</dd>
        <dt>PnL</dt><dd>Unknown</dd>
        <dt>Price</dt><dd>Unavailable. No oracle read.</dd>
      </dl>
      {node.params.map((param) => <p key={param.label} className="muted"><strong style={{ color: "var(--text)" }}>{param.label}.</strong> {param.value}</p>)}
      <label>
        Title
        <input aria-label="Node title" value={node.title} onChange={(event) => onChange({ ...node, title: event.target.value })} />
      </label>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          const data = new FormData(event.currentTarget);
          const targetId = String(data.get("target") ?? "");
          const bps = Number(data.get("bps"));
          const result = proposeRoute(nodes, node.id, targetId, bps);
          if (!result.ok) onRouteNote(`${result.code}: ${result.message}`);
          else {
            const next = result.nodes.find((item) => item.id === node.id);
            if (next) onChange(next);
            onRouteNote("Route stored. The rest of the output is free balance, not another route.");
          }
        }}
      >
        <label>
          Route to
          <select name="target" aria-label="Route target" defaultValue="">
            <option value="" disabled>Choose a later node</option>
            {nodes.filter((item) => item.index > node.index).map((item) => (
              <option key={item.id} value={item.id}>{item.index}. {item.title}</option>
            ))}
          </select>
        </label>
        <label>
          Basis points
          <input name="bps" aria-label="Route basis points" defaultValue="1000" inputMode="numeric" />
        </label>
        <button type="submit" className="btn">Add route</button>
      </form>
      {issues.length > 0 ? (
        <ul className="list">
          {issues.map((issue) => <li key={issue.code + issue.message}>{issue.code}: {issue.message}</li>)}
        </ul>
      ) : <p className="muted">No issue on this node under the rules in this commit.</p>}
    </aside>
  );
}

function Detail({ node, onClose }: { node: FlowNode; onClose: () => void }) {
  const titleId = useId();
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="dialog-back">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <p className="kicker">{node.venue} · {node.chain}</p>
        <h2 id={titleId} style={{ margin: 0 }}>{node.title}</h2>
        <p className="muted">Chart omitted. There is no price history source. A line is not drawn.</p>
        <dl>
          <dt>Configured</dt><dd>Draft parameters only</dd>
          <dt>Observed</dt><dd>Not recorded</dd>
          <dt>Funding, volume, RSI</dt><dd>Unavailable. Those reference panels are not NEAR Flows data.</dd>
          <dt>Audit</dt><dd>Unknown. No versioned evidence is attached.</dd>
        </dl>
        <button type="button" className="btn" onClick={onClose}>Close</button>
      </div>
    </div>
  );
}

function Review({ flowName, nodes, onClose }: { flowName: string; nodes: FlowNode[]; onClose: () => void }) {
  const titleId = useId();
  const issues = validateFlow(nodes);
  const local = exportLocalDraft({ id: "review", vaultId: "local", name: flowName, nodes });
  const contract = contractSerialize(nodes);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="dialog-back">
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <p className="kicker">Review · disarmed</p>
        <h2 id={titleId} style={{ margin: 0 }}>{flowName}</h2>
        <p className="muted">Local draft below keeps every node. It is not submitted. A lens does not remove nodes from it.</p>
        {!contract.ok ? (
          <>
            <h3>Contract serialization blocked</h3>
            <ul className="list">
              {contract.blocked.map((block) => (
                <li key={block.id}>{block.title} · id {block.id} · index {block.index}. {block.reason}</li>
              ))}
            </ul>
            <p className="muted">No contract payload was built. These nodes were not dropped from the local draft.</p>
          </>
        ) : <p className="muted">Every node in this draft has a recipe shape. Submission is still disabled.</p>}
        <ul className="list">
          {issues.slice(0, 8).map((issue) => <li key={issue.code + issue.message}>{issue.code}: {issue.message}</li>)}
        </ul>
        <textarea readOnly value={local} aria-label="Local draft" />
        <p className="muted">Direct evm_call, sol_sign and intents_sign_swap are not flow nodes. A keeper is not granted them here.</p>
        <div style={{ display: "flex", gap: 8 }}>
          <button type="button" className="btn" onClick={onClose}>Back</button>
          <button type="button" className="btn-primary" disabled>Submit testnet request</button>
        </div>
      </div>
    </div>
  );
}

function Demo() {
  const app = useApp();
  const flow = app.flows.find((item) => item.id === app.selectedFlowId) ?? app.flows[0];
  useEffect(() => {
    if (!app.replayPlaying || !app.replay) return;
    const delay = 700 / app.replaySpeed;
    const timer = window.setTimeout(() => {
      if (app.replayCursor >= app.replay!.events.length) app.setPlaying(false);
      else app.setCursor(app.replayCursor + 1);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [app]);
  function onFile(file: File) {
    file.text().then((text) => {
      const doc: ReplayDocument = parseDemoLog(text, file.name);
      app.setReplay(doc);
    });
  }
  const revealed = app.replay?.events.slice(0, app.replayCursor) ?? [];
  const stages = app.replay ? projectLegs(flow.nodes, revealed) : [];
  return (
    <Shell>
      <header className="topbar">
        <ModeBadge />
        <span className="badge">LOCAL FORK LIVE · no feed</span>
        <span className="spacer" />
      </header>
      <div className="page">
        <h1 style={{ margin: 0, fontSize: 24 }}>Four-leg fork</h1>
        <p className="muted">
          scripts/demo/run.sh was not started. contracts/target/demo.log is not in the tree.
          Forks in that script are copies of mainnet state, not mainnet execution. Playback never signs or broadcasts.
          Pause only freezes this view.
        </p>
        <div className="toolbar" style={{ padding: 0 }}>
          <input aria-label="Import DEMO log" type="file" accept=".log,.txt" onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) onFile(file);
          }} />
          <button type="button" className="btn" disabled={!app.replay} onClick={() => app.setPlaying(!app.replayPlaying)}>{app.replayPlaying ? "Pause" : "Play"}</button>
          <button type="button" className="btn" disabled={!app.replay} onClick={() => app.resetReplay()}>Reset</button>
          {([0.5, 1, 2] as const).map((speed) => (
            <button key={speed} type="button" className="btn" disabled={!app.replay} aria-pressed={app.replaySpeed === speed} onClick={() => app.setSpeed(speed)}>{speed}× display</button>
          ))}
        </div>
        {!app.replay ? (
          <article className="card">
            <h2>No recorded run</h2>
            <p>Import a log whose lines start with DEMO|. Until then every node stays at awaiting evidence. There is no successful sample loaded.</p>
          </article>
        ) : (
          <article className="card">
            <h2>Imported log</h2>
            <p>{app.replay.timing} Legs: {app.replay.legs.length}. Rows: {app.replay.events.length}. Display cursor: {app.replayCursor}. No original event time is shown.</p>
            {app.replay.warnings.map((warning) => <p key={warning}>{warning}</p>)}
            <ol className="list">
              {revealed.map((event) => (
                <li key={event.id}>
                  <span className="num">row {event.sourceRow}</span> {event.leg ? `leg ${event.leg}` : "no leg"} · {event.evidenced.length ? event.evidenced.join(", ") : "no stage marker"} · not recorded: {event.notRecorded.join(", ")}
                  <div>{event.raw}</div>
                </li>
              ))}
            </ol>
          </article>
        )}
        <div className="card" style={{ height: 720, padding: 0, overflow: "hidden" }}>
          <FlowBoard nodes={visibleNodes(flow.nodes, { chain: "all", service: "all" })} stages={stages} selectedId={app.selectedNodeId} onSelect={app.selectNode} onDetails={app.selectNode} onMove={app.moveNode} onConnectEnds={() => undefined} />
        </div>
        <StageLegend />
      </div>
    </Shell>
  );
}

function StageLegend() {
  const items: { id: Stage | "none"; text: string }[] = [
    { id: "none", text: "Awaiting evidence is not success." },
    { id: "signed", text: "Signed is not sent." },
    { id: "sent", text: "Sent is not recorded. demo.rs never prints a sent marker." },
    { id: "landed", text: "Landed only for executed on the fork or executed:. It does not fill earlier gaps." },
    { id: "reported", text: "Reported is not an independent balance." },
    { id: "failed", text: "REFUSED stays failed." },
  ];
  return (
    <ul className="list">
      {items.map((item) => <li key={item.id}>{item.text}</li>)}
    </ul>
  );
}

function Settings() {
  return (
    <Shell>
      <header className="topbar"><ModeBadge /></header>
      <div className="page">
        <h1 style={{ margin: 0, fontSize: 24 }}>Vault settings</h1>
        <div className="grid">
          <article className="card">
            <h2>Stealth</h2>
            <p>A stealth draft is owned by a key, not by the account that would pay the factory. This screen does not generate or store a key. Stealth is not secrecy of on-chain transfers.</p>
          </article>
          <article className="card">
            <h2>Graph visibility</h2>
            <p>Public / private is read-only. Private would not hide a public transaction. hide_flow exists on this commit and is not called.</p>
            <div className="toolbar" style={{ padding: 0 }}>
              <button type="button" className="btn" disabled aria-disabled="true">Public</button>
              <button type="button" className="btn" disabled aria-disabled="true">Private</button>
            </div>
          </article>
          <article className="card">
            <h2>Keeper</h2>
            <p>Run your own keeper: the software is free. Network and hosting cost are not priced here. Paid provider: unknown. No online light.</p>
            <button type="button" className="btn" disabled>Choose a paid keeper</button>
          </article>
          <article className="card">
            <h2>Close</h2>
            <p>User report: owner or keeper, with holdings checks, and closed names stay closed. This commit’s close and is_closed were not re-checked as that new predicate. The action stays disarmed.</p>
            <button type="button" className="btn" disabled>Review close</button>
          </article>
          <article className="card">
            <h2>Share as template</h2>
            <p>Deferred. Audit status: Unknown. Nothing is marked audited.</p>
            <button type="button" className="btn" disabled>Share template</button>
          </article>
          <article className="card">
            <h2>Direct signing</h2>
            <p>evm_call, sol_sign_swap, sol_sign_tx and intents_sign_swap are not on the canvas. This commit’s comments still allow a keeper to call them. The UI does not offer that path.</p>
          </article>
        </div>
        <article className="card">
          <h2>Checked against {SOURCE_COMMIT.slice(0, 7)}</h2>
          <ul className="list">
            {ABI.notes.map((note) => (
              <li key={note.topic}><strong>{note.topic}.</strong> {note.ui}</li>
            ))}
          </ul>
        </article>
      </div>
    </Shell>
  );
}

function Feed() {
  return (
    <Shell>
      <header className="topbar"><ModeBadge /></header>
      <div className="page">
        <h1 style={{ margin: 0, fontSize: 24 }}>Public feed</h1>
        <article className="card">
          <h2>Unavailable</h2>
          <p>No public graph index is in this commit. Sharing and template audit stay deferred. This page does not list invented flows.</p>
        </article>
      </div>
    </Shell>
  );
}

export function App() {
  return (
    <Routes>
      <Route path="/" element={<Overview />} />
      <Route path="/vaults" element={<Vaults />} />
      <Route path="/flows" element={<Flows />} />
      <Route path="/flows/:id" element={<FlowWorkspace />} />
      <Route path="/demo" element={<Demo />} />
      <Route path="/settings" element={<Settings />} />
      <Route path="/feed" element={<Feed />} />
    </Routes>
  );
}
