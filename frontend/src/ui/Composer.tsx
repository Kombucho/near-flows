import { useEffect, useState } from "react";
import { DEMO_ACCOUNT } from "../demo/fixtures";
import { actionsFor, composePlan, draftFromPrompt } from "../model/compose";
import type { ChainId, FlowNode } from "../model/flow";
import { CHAIN_PINS } from "../model/shell";

const STEPS = ["Chain", "Protocol", "Amount", "Action"] as const;

export function Composer({
  demo,
  startIndex,
  origin,
  onClose,
  onCreate,
}: {
  demo: boolean;
  startIndex: number;
  origin: { x: number; y: number };
  onClose: () => void;
  onCreate: (nodes: FlowNode[]) => void;
}) {
  const [step, setStep] = useState(0);
  const [chain, setChain] = useState<ChainId>("solana");
  const [protocol, setProtocol] = useState("meteora");
  const [pct, setPct] = useState(25);
  const [plan, setPlan] = useState("12");
  const [actionId, setActionId] = useState("dlmm");
  const [prompt, setPrompt] = useState("");
  const [promptNote, setPromptNote] = useState<string | null>(null);
  const protocols = CHAIN_PINS.find((pin) => pin.id === chain)?.protocols ?? [];
  const actions = actionsFor(chain, protocol);
  const balance = demo ? DEMO_ACCOUNT.holdings.find((holding) => holding.symbol === "NEAR")?.amount ?? "420" : null;
  const amount = demo && balance ? String(Math.round((Number(balance.replace(/,/g, "")) * pct) / 100)) : plan;

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => { if (event.key === "Escape") onClose(); };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  function applyPrompt() {
    const draft = draftFromPrompt(prompt);
    if (!draft) {
      setPromptNote("No template for that. Nothing was added.");
      return;
    }
    setChain(draft.chain);
    setProtocol(draft.protocol);
    setActionId(draft.actionId);
    setStep(2);
    setPromptNote("Template draft. Nothing is sent.");
  }

  return (
    <section className="panel composer" role="dialog" aria-label="Compose node">
      <div className="desk-head">
        <h2>Compose</h2>
        <button type="button" onClick={onClose} aria-label="Close compose">Close</button>
      </div>
      <ol className="steps-nav">
        {STEPS.map((label, index) => (
          <li key={label} className={index === step ? "on" : ""}>{index + 1} {label}</li>
        ))}
      </ol>
      {step === 0 ? (
        <div className="choice">
          {CHAIN_PINS.map((pin) => (
            <button key={pin.id} type="button" className="menu-item" aria-pressed={chain === pin.id} onClick={() => { setChain(pin.id); setProtocol(pin.protocols[0]?.id ?? ""); setStep(1); }}>
              <strong>{pin.label}</strong>
            </button>
          ))}
        </div>
      ) : null}
      {step === 1 ? (
        <div className="choice">
          {protocols.map((item) => (
            <button key={item.id} type="button" className="menu-item" aria-pressed={protocol === item.id} onClick={() => { setProtocol(item.id); setActionId(actionsFor(chain, item.id)[0]?.id ?? ""); setStep(2); }}>
              <strong>{item.label}</strong>
              <small>{item.flowNode ? "Flow node" : "Not a flow node"}</small>
            </button>
          ))}
          <button type="button" className="chip-btn" onClick={() => setStep(0)}>Back</button>
        </div>
      ) : null}
      {step === 2 ? (
        <div className="stack-form">
          <p>{demo ? <><span className="chip demo-tag">Demo</span> Vault {balance} NEAR</> : "Vault balance unknown"}</p>
          {demo ? (
            <>
              <input aria-label="Amount percent" type="range" min={1} max={100} value={pct} onChange={(event) => setPct(Number(event.target.value))} />
              <div className="row">
                {[10, 25, 50, 100].map((value) => (
                  <button key={value} type="button" className="chip-btn" aria-pressed={pct === value} onClick={() => setPct(value)}>{value}%</button>
                ))}
              </div>
              <p className="num">{amount} NEAR</p>
            </>
          ) : (
            <>
              <p className="quiet">Percent of the vault is unknown. Set a plan amount.</p>
              <input aria-label="Plan amount" inputMode="decimal" value={plan} onChange={(event) => setPlan(event.target.value)} />
            </>
          )}
          <div className="row">
            <button type="button" className="chip-btn" onClick={() => setStep(1)}>Back</button>
            <button type="button" className="chip-btn" onClick={() => setStep(3)}>Next</button>
          </div>
        </div>
      ) : null}
      {step === 3 ? (
        <div className="choice">
          {actions.map((item) => (
            <button key={item.id} type="button" className="menu-item" aria-pressed={actionId === item.id} onClick={() => setActionId(item.id)}>
              <strong>{item.title}</strong>
              <small>{item.serializable ? item.venue : "Local"}</small>
            </button>
          ))}
          <p className="quiet">Other chains add Wrap NEAR, an Intents hop, and that chain’s vault. Nothing is signed.</p>
          <div className="row">
            <button type="button" className="chip-btn" onClick={() => setStep(2)}>Back</button>
            <button
              type="button"
              className="chip-btn"
              onClick={() => {
                const nodes = composePlan({ chain, protocol, actionId, amountNear: amount, startIndex, origin });
                if (nodes.length) onCreate(nodes);
              }}
            >
              Add plan
            </button>
          </div>
        </div>
      ) : null}
      <label>
        Tell it what to do
        <input aria-label="Agent prompt" value={prompt} placeholder="Open an LP on Meteora" onChange={(event) => setPrompt(event.target.value)} />
      </label>
      <button type="button" className="chip-btn" onClick={applyPrompt}>Draft from template</button>
      {promptNote ? <p className="quiet">{promptNote}</p> : null}
    </section>
  );
}
