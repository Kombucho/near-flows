import { useEffect, useId, useMemo, useState } from "react";
import { SkulletProvider } from "skullet/react";
import { DEMO_ACCOUNT } from "../demo/fixtures";
import { ABI } from "../model/abi";
import { contractSerialize, exportLocalDraft, importLocalDraft, proposeRoute, remainderBps, validateFlow, visibleNodes, type ChainId, type FlowNode } from "../model/flow";
import { SOURCE_COMMIT } from "../model/limits";
import { parseDemoLog, projectLegs } from "../model/replay";
import { CHAIN_PINS, chainPin, paletteGroups } from "../model/shell";
import { useApp, type Surface, type VaultDraft } from "../state";
import { ChartView } from "./ChartView";
import { ChainDesk, PortfolioDesk, ProtocolDesk } from "./Desk";
import { DemoTag } from "./bits";
import { FlowBoard } from "./FlowBoard";

type Panel = "flows" | "nodes" | "connect" | "settings" | "review" | "inspector" | null;
type ThemeName = "light" | "dark";

const THEME_KEY = "near-flows.theme";
const DEMO_KEY = "near-flows.demo";

function readTheme(): ThemeName {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

export function Workspace() {
  const app = useApp();
  const [theme, setTheme] = useState<ThemeName>(readTheme);
  const [demo, setDemo] = useState(() => localStorage.getItem(DEMO_KEY) === "1");
  const [panel, setPanel] = useState<Panel>(null);
  const [note, setNote] = useState<string | null>(null);
  const [flowName, setFlowName] = useState("");
  const [vaultStep, setVaultStep] = useState<"closed" | "edit" | "review">("closed");
  const [vaultLabel, setVaultLabel] = useState("");
  const [vaultKind, setVaultKind] = useState<VaultDraft["kind"]>("named");

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  useEffect(() => {
    localStorage.setItem(DEMO_KEY, demo ? "1" : "0");
  }, [demo]);

  useEffect(() => {
    if (!note) return;
    const timer = window.setTimeout(() => setNote(null), 4200);
    return () => window.clearTimeout(timer);
  }, [note]);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setPanel(null);
        setVaultStep("closed");
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  useEffect(() => {
    if (!app.replayPlaying || !app.replay) return;
    const delay = 700 / app.replaySpeed;
    const timer = window.setTimeout(() => {
      if (!app.replay || app.replayCursor >= app.replay.events.length) app.setPlaying(false);
      else app.setCursor(app.replayCursor + 1);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [app]);

  const vaultFlows = app.flows.filter((flow) => flow.vaultId === app.selectedVaultId);
  const flow = vaultFlows.find((item) => item.id === app.selectedFlowId) ?? null;
  const chain = chainPin(app.lens.chain);
  const visible = useMemo(() => (flow ? visibleNodes(flow.nodes, app.lens) : []), [flow, app.lens]);
  const selected = flow?.nodes.find((node) => node.id === app.selectedNodeId) ?? null;
  const revealed = app.replay ? app.replay.events.slice(0, app.replayCursor) : [];
  const stages = flow && app.surface === "strategies" && app.replay ? projectLegs(flow.nodes, revealed) : [];
  const groups = paletteGroups(app.lens.chain, app.lens.service);
  const protocol = chain?.protocols.find((item) => item.id === app.lens.service) ?? null;

  function toggle(next: Exclude<Panel, null>) {
    setVaultStep("closed");
    setPanel((current) => (current === next ? null : next));
  }

  function pickChain(id: ChainId) {
    if (app.lens.chain === id) {
      app.setLens({ chain: "all", service: "all" });
      app.setSurface("portfolio");
    } else {
      app.setLens({ chain: id, service: "all" });
      app.setSurface("chain");
    }
  }

  function pickProtocol(id: string) {
    if (!chain) return;
    if (app.lens.service === id) {
      app.setLens({ chain: chain.id, service: "all" });
      app.setSurface("chain");
    } else {
      app.setLens({ chain: chain.id, service: id });
      app.setSurface("protocol");
    }
  }

  function openStrategy() {
    app.setSurface("strategies");
  }

  function saveDraft() {
    if (!flow) return;
    localStorage.setItem("near-flows.local-draft-file", exportLocalDraft(flow));
    setNote("Draft saved");
  }

  function onConnect(sourceId: string, targetId: string) {
    if (!flow) return;
    const result = proposeRoute(flow.nodes, sourceId, targetId, 1000);
    if (!result.ok) setNote(result.message);
    else {
      app.replaceNodes(result.nodes);
      setNote("Route 10%");
    }
  }

  const strategies = app.surface === "strategies";
  const showBoard = strategies && flow;

  return (
    <SkulletProvider
      mode={theme}
      theme={{
        radius: "8px",
        animation: "shimmer",
        dark: { base: "#1c1c1c", highlight: "#2a2a2e" },
        light: { base: "#e4e4e7", highlight: "#f4f4f5" },
      }}
    >
    <div className={strategies ? "studio board-mode" : chain ? "studio tall" : "studio"}>
      {showBoard ? (
        <FlowBoard
          nodes={visible}
          stages={stages}
          selectedId={selected?.id ?? null}
          theme={theme}
          focusKey={`${flow.id}:${app.lens.chain}:${app.lens.service}`}
          onSelect={(id) => app.selectNode(id || null)}
          onDetails={(id) => {
            app.selectNode(id);
            setPanel("inspector");
          }}
          onMove={app.moveNode}
          onConnectEnds={onConnect}
        />
      ) : strategies ? (
        <div className="canvas-wrap bare">
          <div className="pnl">
            <h1>{app.vaults.find((vault) => vault.id === app.selectedVaultId)?.label}</h1>
            <p>No strategy</p>
          </div>
        </div>
      ) : (
        <main className="sheet">
          {app.surface === "chart" ? (
            <ChartView
              demo={demo}
              replay={app.replay}
              playing={app.replayPlaying}
              cursor={app.replay ? app.replayCursor || app.replay.events.length : 0}
              speed={app.replaySpeed}
              onImport={(file) => file.text().then((text) => app.setReplay(parseDemoLog(text, file.name)))}
              onPlay={() => app.setPlaying(!app.replayPlaying)}
              onReset={() => app.resetReplay()}
              onSpeed={app.setSpeed}
            />
          ) : null}
          {app.surface === "portfolio" ? <PortfolioDesk demo={demo} nodes={flow?.nodes ?? []} activity={app.replay} /> : null}
          {app.surface === "chain" && chain ? (
            <ChainDesk demo={demo} chain={chain} nodes={flow?.nodes ?? []} onOpen={pickProtocol} />
          ) : null}
          {app.surface === "protocol" && chain && protocol ? (
            <ProtocolDesk
              demo={demo}
              chain={chain}
              protocol={protocol}
              nodes={flow?.nodes ?? []}
              replay={app.replay}
              onAdd={(id) => {
                app.addFromPalette(id);
                setNote("Added to the strategy");
              }}
              onOpenStrategy={openStrategy}
            />
          ) : null}
        </main>
      )}

      <header className="chrome">
        <div className="stack">
          <div className="island">
            <span className="mark" aria-hidden="true">流</span>
            {demo ? <span className="demo-badge" role="status">DEMO</span> : null}
            <div className="pins" role="toolbar" aria-label="Chains">
              {CHAIN_PINS.map((pin) => (
                <button
                  key={pin.id}
                  type="button"
                  className="pin"
                  data-chain={pin.id}
                  aria-pressed={app.lens.chain === pin.id}
                  onClick={() => pickChain(pin.id)}
                >
                  <span className="dot" />
                  {pin.label}
                </button>
              ))}
            </div>
          </div>
          <div className="island">
            <div className="pins" role="toolbar" aria-label="Views">
              <ViewButton current={app.surface} id="portfolio" onPick={app.setSurface}>Portfolio</ViewButton>
              <ViewButton current={app.surface} id="strategies" onPick={app.setSurface}>Strategies</ViewButton>
              <ViewButton current={app.surface} id="chart" onPick={app.setSurface}>Chart</ViewButton>
            </div>
            {chain ? (
              <div className="pins" role="toolbar" aria-label={`${chain.label} protocols`}>
                {chain.protocols.map((item) => (
                  <button
                    key={item.id}
                    type="button"
                    className="pin protocol"
                    aria-pressed={app.lens.service === item.id}
                    onClick={() => pickProtocol(item.id)}
                  >
                    {item.label}
                  </button>
                ))}
              </div>
            ) : null}
          </div>
        </div>

        <div className="island right">
          <button type="button" className="flow-name" onClick={() => toggle("flows")}>
            {flow?.name ?? "No flow"}
          </button>
          <button type="button" className="plus" aria-label="Choose flow" aria-expanded={panel === "flows"} onClick={() => toggle("flows")}>
            +
          </button>
          <button type="button" className="icon-btn" aria-label={theme === "dark" ? "Use light theme" : "Use dark theme"} onClick={() => setTheme((value) => (value === "dark" ? "light" : "dark"))}>
            {theme === "dark" ? "Light" : "Dark"}
          </button>
          <button type="button" className="chip-btn" aria-pressed={demo} onClick={() => setDemo((value) => !value)}>
            Try demo
          </button>
          <button type="button" className="connect-btn" aria-expanded={panel === "connect"} onClick={() => toggle("connect")}>
            Connect NEAR
          </button>
          <button type="button" className="icon-btn" aria-label="Settings" aria-expanded={panel === "settings"} onClick={() => toggle("settings")}>
            ···
          </button>
        </div>
      </header>

      {strategies && app.replay ? (
        <span className="replay-flag chip replay">Replay</span>
      ) : null}
      {strategies && demo ? (
        <aside className="demo-runs" aria-label="Demo runs">
          <div className="desk-head">
            <h2>Run history</h2>
            <DemoTag />
          </div>
          <ul className="feed">
            {DEMO_ACCOUNT.strategies.flatMap((strategy) => strategy.runs.map((run) => (
              <li key={strategy.name + run.when}>
                <span>
                  <strong>{strategy.name}</strong>
                  <div className="quiet">{run.when}</div>
                </span>
                <span>{run.result}</span>
              </li>
            )))}
          </ul>
          <span className="chip demo-keeper">{DEMO_ACCOUNT.keeper}</span>
        </aside>
      ) : null}

      {note ? <p className="toast" role="status">{note}</p> : null}

      {strategies ? <button
        type="button"
        className="nodes-chip"
        aria-expanded={panel === "nodes"}
        onClick={() => toggle("nodes")}
      >
        <span>
          <strong>Nodes</strong>
          <small>{protocol?.label ?? chain?.label ?? "All chains"}</small>
        </span>
      </button> : null}

      {panel === "flows" ? (
        <section className="panel panel-flows" aria-label="Flows">
          <h2>Flows</h2>
          <ul>
            {vaultFlows.map((item) => (
              <li key={item.id}>
                <button
                  type="button"
                  className="menu-item"
                  aria-current={item.id === flow?.id ? "true" : undefined}
                  onClick={() => {
                    app.selectFlow(item.id);
                    setPanel(null);
                  }}
                >
                  <strong>{item.name}</strong>
                  <small>{item.nodes.length}</small>
                </button>
              </li>
            ))}
          </ul>
          <form
            className="stack-form"
            onSubmit={(event) => {
              event.preventDefault();
              app.createFlow(flowName);
              setFlowName("");
              setPanel(null);
            }}
          >
            <input aria-label="New flow name" placeholder="New flow" value={flowName} onChange={(event) => setFlowName(event.target.value)} />
            <button type="submit" disabled={flowName.trim().length < 2}>Create</button>
          </form>
          <h2>Vaults</h2>
          <ul>
            {app.vaults.map((vault) => (
              <li key={vault.id}>
                <button
                  type="button"
                  className="menu-item"
                  aria-current={vault.id === app.selectedVaultId ? "true" : undefined}
                  onClick={() => app.selectVault(vault.id)}
                >
                  <strong>{vault.label}</strong>
                  <small>{vault.kind === "stealth" ? "Stealth" : "Named"}</small>
                </button>
              </li>
            ))}
          </ul>
          {vaultStep === "closed" ? (
            <button type="button" onClick={() => setVaultStep("edit")}>New vault</button>
          ) : null}
          {vaultStep === "edit" ? (
            <form
              className="stack-form"
              onSubmit={(event) => {
                event.preventDefault();
                setVaultStep("review");
              }}
            >
              <input aria-label="Vault label" value={vaultLabel} onChange={(event) => setVaultLabel(event.target.value)} placeholder="Label" />
              <select aria-label="Vault kind" value={vaultKind} onChange={(event) => setVaultKind(event.target.value as VaultDraft["kind"])}>
                <option value="named">Named</option>
                <option value="stealth">Stealth</option>
              </select>
              <div className="row">
                <button type="button" onClick={() => setVaultStep("closed")}>Cancel</button>
                <button type="submit" disabled={vaultLabel.trim().length < 2}>Review</button>
              </div>
            </form>
          ) : null}
          {vaultStep === "review" ? (
            <div className="stack-form">
              <p>{vaultKind} · {vaultLabel.trim()}</p>
              <p className="quiet">Kept on this device.</p>
              <div className="row">
                <button type="button" onClick={() => setVaultStep("edit")}>Back</button>
                <button type="button" disabled>Send</button>
                <button
                  type="button"
                  onClick={() => {
                    app.addVault({ id: `local-${Date.now()}`, label: vaultLabel.trim(), kind: vaultKind, loginLabel: app.loginLabel });
                    setVaultLabel("");
                    setVaultStep("closed");
                  }}
                >
                  Keep draft
                </button>
              </div>
            </div>
          ) : null}
          <div className="row">
            <button type="button" disabled={!flow} onClick={() => { saveDraft(); }}>Save</button>
            <label className="file-btn">
              Import
              <input
                aria-label="Import local draft"
                type="file"
                accept="application/json,.json"
                hidden
                onChange={(event) => {
                  const file = event.target.files?.[0];
                  event.target.value = "";
                  if (!file || !flow) return;
                  file.text().then((text) => {
                    try {
                      app.replaceNodes(importLocalDraft(text).nodes);
                      setNote("Draft imported");
                    } catch {
                      setNote("Import refused");
                    }
                  });
                }}
              />
            </label>
            <button type="button" disabled={!flow} onClick={() => setPanel("review")}>Review</button>
          </div>
        </section>
      ) : null}

      {panel === "nodes" ? (
        <section className="panel panel-nodes" aria-label="Nodes">
          <h2>Nodes</h2>
          {groups.map((group) => (
            <div key={group.id} className="group">
              <h3>{group.label}</h3>
              {group.items.length === 0 ? <p className="quiet">Owner-signed swap. Not a flow node.</p> : null}
              <ul>
                {group.items.map((item) => (
                  <li key={item.id}>
                    <button type="button" className="menu-item" disabled={!flow} onClick={() => flow && app.addFromPalette(item.id)}>
                      <strong>{item.title}</strong>
                      <small>{item.serializable ? item.venue : "Local"}</small>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          ))}
        </section>
      ) : null}

      {panel === "inspector" && selected && flow ? (
        <Inspector
          node={selected}
          nodes={flow.nodes}
          onChange={app.updateVisibleNode}
          onClose={() => setPanel(null)}
          onNote={setNote}
        />
      ) : null}

      {panel === "connect" ? <ConnectPanel demo={demo} onDemo={() => setDemo((value) => !value)} onClose={() => setPanel(null)} /> : null}
      {panel === "settings" ? <SettingsPanel onClose={() => setPanel(null)} /> : null}
      {panel === "review" && flow ? <ReviewPanel name={flow.name} nodes={flow.nodes} onClose={() => setPanel(null)} /> : null}
    </div>
    </SkulletProvider>
  );
}

function ViewButton({ current, id, onPick, children }: { current: Surface; id: Surface; onPick: (surface: Surface) => void; children: string }) {
  return (
    <button type="button" className="pin view" aria-pressed={current === id} onClick={() => onPick(id)}>
      {children}
    </button>
  );
}

function Inspector({
  node,
  nodes,
  onChange,
  onClose,
  onNote,
}: {
  node: FlowNode;
  nodes: FlowNode[];
  onChange: (node: FlowNode) => void;
  onClose: () => void;
  onNote: (note: string) => void;
}) {
  const titleId = useId();
  const left = remainderBps(node);
  const issues = validateFlow(nodes).filter((issue) => issue.nodeId === node.id);
  return (
    <section className="panel panel-inspector" role="dialog" aria-labelledby={titleId}>
      <div className="row spread">
        <h2 id={titleId}>{node.title}</h2>
        <button type="button" onClick={onClose} aria-label="Close inspector">Close</button>
      </div>
      <p className="quiet">{node.venue}</p>
      <dl>
        <dt>In</dt><dd className="num">{node.input}</dd>
        <dt>Out</dt><dd className="num">{node.output ?? "none"}</dd>
        <dt>Free</dt><dd className="num">{left == null ? "No output" : `${left} bps`}</dd>
        <dt>PnL</dt><dd>Unavailable</dd>
      </dl>
      <label>
        Title
        <input aria-label="Node title" value={node.title} onChange={(event) => onChange({ ...node, title: event.target.value })} />
      </label>
      <form
        key={node.id}
        className="stack-form"
        onSubmit={(event) => {
          event.preventDefault();
          const data = new FormData(event.currentTarget);
          const result = proposeRoute(nodes, node.id, String(data.get("target") ?? ""), Number(data.get("bps")));
          if (!result.ok) onNote(result.message);
          else {
            const next = result.nodes.find((item) => item.id === node.id);
            if (next) onChange(next);
            onNote("Route stored");
          }
        }}
      >
        <select name="target" aria-label="Route target" defaultValue="">
          <option value="" disabled>Later node</option>
          {nodes.filter((item) => item.index > node.index).map((item) => (
            <option key={item.id} value={item.id}>{item.title}</option>
          ))}
        </select>
        <input name="bps" aria-label="Route basis points" defaultValue="1000" inputMode="numeric" />
        <button type="submit">Add route</button>
      </form>
      {issues.length > 0 ? (
        <ul className="issues">
          {issues.map((issue) => <li key={issue.code}>{issue.code}</li>)}
        </ul>
      ) : null}
      {!node.serializable ? <p className="quiet">Blocks a contract payload.</p> : null}
    </section>
  );
}

function ConnectPanel({ demo, onDemo, onClose }: { demo: boolean; onDemo: () => void; onClose: () => void }) {
  const titleId = useId();
  return (
    <section className="panel panel-connect" role="dialog" aria-labelledby={titleId}>
      <div className="row spread">
        <h2 id={titleId}>Connect NEAR</h2>
        <button type="button" onClick={onClose} aria-label="Close connect">Close</button>
      </div>
      <p>No wallet connector in this build.</p>
      <p className="quiet">Drafts stay on this device.</p>
      <button type="button" className="chip-btn" aria-pressed={demo} onClick={onDemo}>Try demo</button>
    </section>
  );
}

function SettingsPanel({ onClose }: { onClose: () => void }) {
  const titleId = useId();
  return (
    <section className="panel panel-settings" role="dialog" aria-labelledby={titleId}>
      <div className="row spread">
        <h2 id={titleId}>Settings</h2>
        <button type="button" onClick={onClose} aria-label="Close settings">Close</button>
      </div>
      <p className="quiet num">Source {SOURCE_COMMIT.slice(0, 7)}</p>
      <dl>
        <dt>Factory</dt><dd>{ABI.deployment.factory}</dd>
        <dt>Brain</dt><dd>{ABI.deployment.brain}</dd>
        <dt>Board</dt><dd>{ABI.deployment.board}</dd>
      </dl>
      <ul className="issues">
        <li>Hyperliquid is not in this build.</li>
        <li>Jupiter is an owner-signed swap, not a flow node.</li>
        <li>The EVM pin is Robinhood Chain in this draft. Base templates are not on the canvas.</li>
        {ABI.notes.map((item) => <li key={item.topic}><strong>{item.topic}.</strong> {item.detail}</li>)}
      </ul>
    </section>
  );
}

function ReviewPanel({ name, nodes, onClose }: { name: string; nodes: FlowNode[]; onClose: () => void }) {
  const titleId = useId();
  const contract = useMemo(() => contractSerialize(nodes), [nodes]);
  const local = exportLocalDraft({ id: "review", vaultId: "local", name, nodes });
  return (
    <section className="panel panel-review" role="dialog" aria-labelledby={titleId}>
      <div className="row spread">
        <h2 id={titleId}>{name}</h2>
        <button type="button" onClick={onClose} aria-label="Close review">Close</button>
      </div>
      {!contract.ok ? (
        <ul className="issues">
          {contract.blocked.map((block) => (
            <li key={block.id}>{block.title} · index {block.index}. {block.reason}</li>
          ))}
        </ul>
      ) : (
        <p className="quiet">Payload can be built. Send is off.</p>
      )}
      <textarea readOnly value={local} aria-label="Local draft" />
      <div className="row">
        <button type="button" onClick={onClose}>Back</button>
        <button type="button" disabled>Send</button>
      </div>
    </section>
  );
}
