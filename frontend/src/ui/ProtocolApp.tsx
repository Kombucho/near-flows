import { useState } from "react";
import { draftFromPrompt } from "../model/compose";
import type { PoolRow } from "../live/quotes";

const SHAPES = ["Spot", "Curve", "Bid-ask"] as const;

export function ProtocolApp({
  service,
  demo,
  pools,
}: {
  service: string;
  demo: boolean;
  pools: { status: "live" | "unavailable"; pools: PoolRow[] };
}) {
  const [shape, setShape] = useState<(typeof SHAPES)[number]>("Curve");
  const [pool, setPool] = useState("");
  const [when, setWhen] = useState("fees above the plan");
  const [thenDo, setThenDo] = useState("sweep");
  const [prompt, setPrompt] = useState("");
  const [note, setNote] = useState<string | null>(null);
  const chosen = pools.pools.find((item) => item.address === pool) ?? pools.pools[0];

  return (
    <div className="mini">
      <p className="eyebrow">In this node</p>
      {service === "meteora" ? (
        <>
          <h3>Meteora DLMM</h3>
          {pools.status === "live" && chosen ? (
            <>
              <span className="chip ok">live</span>
              <label>
                Pool
                <select aria-label="Meteora pool" value={chosen.address} onChange={(event) => setPool(event.target.value)}>
                  {pools.pools.map((item) => <option key={item.address} value={item.address}>{item.name}</option>)}
                </select>
              </label>
              <p className="quiet">{chosen.name} · APY {chosen.apy == null ? "unknown" : `${chosen.apy.toFixed(1)}%`} · TVL {chosen.tvl == null ? "unknown" : Math.round(chosen.tvl).toLocaleString("en-US")}</p>
            </>
          ) : (
            <p className="empty-well">Pools unavailable{demo ? ". Demo does not invent an APR." : ""}</p>
          )}
          <div className="row">
            {SHAPES.map((item) => (
              <button key={item} type="button" className="chip-btn" aria-pressed={shape === item} onClick={() => setShape(item)}>{item}</button>
            ))}
          </div>
          <label>Bin range <input aria-label="Bin range" defaultValue="±25" /></label>
          <label>Deposit <input aria-label="Deposit" defaultValue="the plan amount" /></label>
          <label className="row"><input type="checkbox" defaultChecked /> Sweep fees</label>
          <p className="quiet">Rebalance when the active bin leaves the range. Not sent.</p>
        </>
      ) : null}
      {service === "pump" ? (
        <>
          <h3>pump.fun</h3>
          <label>Mint <input aria-label="Mint" placeholder="mint" /></label>
          <label>SOL in <input aria-label="SOL in" defaultValue="the landed SOL" /></label>
          <p className="quiet">Buy on the curve. Not sent.</p>
        </>
      ) : null}
      {service === "jupiter" ? (
        <>
          <h3>Jupiter</h3>
          <p>Owner-signed swap. Not a flow node.</p>
          <label>In <input aria-label="Jupiter in" defaultValue="SOL" /></label>
          <label>Out <input aria-label="Jupiter out" defaultValue="USDC" /></label>
          <p className="quiet">Jupiter’s price API was not usable from this preview.</p>
        </>
      ) : null}
      {service === "rhea" ? (
        <>
          <h3>Rhea</h3>
          <label>Pair <input aria-label="Rhea pair" defaultValue="wNEAR → MEME" /></label>
          <label>Slippage <input aria-label="Slippage" defaultValue="1%" /></label>
        </>
      ) : null}
      {service === "linear" ? (
        <>
          <h3>LiNEAR</h3>
          <label>Stake <input aria-label="Stake amount" defaultValue="the plan amount" /></label>
          <p className="quiet">Liquid stake. Not sent.</p>
        </>
      ) : null}
      {service === "uniswap-v4" ? (
        <>
          <h3>Uniswap v4</h3>
          <label>Pair <input aria-label="v4 pair" defaultValue="ETH → NVDA" /></label>
          <label>Share <input aria-label="v4 share" defaultValue="the landed ETH" /></label>
        </>
      ) : null}
      {service === "wnear" || service === "intents" || service === "vault" || service === "stonk" ? (
        <p className="quiet">{service === "intents" ? "Hop stays in the local plan and blocks a contract payload." : "Plan step. Not sent."}</p>
      ) : null}
      <h3>When, then</h3>
      <div className="row">
        <input aria-label="When" value={when} onChange={(event) => setWhen(event.target.value)} />
        <input aria-label="Do" value={thenDo} onChange={(event) => setThenDo(event.target.value)} />
      </div>
      <label>
        Tell it what to do
        <input aria-label="Node prompt" value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder="Stake with LiNEAR" />
      </label>
      <button
        type="button"
        className="chip-btn"
        onClick={() => {
          const draft = draftFromPrompt(prompt);
          setNote(draft ? `Template: ${draft.protocol}. Nothing is sent.` : "No template for that.");
        }}
      >
        Draft from template
      </button>
      {note ? <p className="quiet">{note}</p> : null}
    </div>
  );
}
