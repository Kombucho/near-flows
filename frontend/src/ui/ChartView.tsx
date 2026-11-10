import { useEffect, useRef, useState } from "react";
import { mount, register } from "pele/core";
import xychart from "pele/diagrams/xychart";
import { DEMO_ACCOUNT, DEMO_RANGES, type DemoRange } from "../demo/fixtures";
import { portfolioPnlChart, replayStageChart } from "../model/chart";
import type { ReplayDocument } from "../model/replay";
import { DemoTag, Spark, usd } from "./bits";

register(xychart);

export function ChartView({
  demo,
  replay,
  onImport,
  playing,
  cursor,
  onPlay,
  onReset,
  onSpeed,
  speed,
}: {
  demo: boolean;
  replay: ReplayDocument | null;
  onImport: (file: File) => void;
  playing: boolean;
  cursor: number;
  onPlay: () => void;
  onReset: () => void;
  onSpeed: (speed: 0.5 | 1 | 2) => void;
  speed: 0.5 | 1 | 2;
}) {
  const revealed = replay?.events.slice(0, cursor) ?? [];
  const chart = replay ? replayStageChart(revealed.length > 0 ? revealed : replay.events) : null;
  const [range, setRange] = useState<DemoRange>("1W");
  const pnl = portfolioPnlChart(demo, range);
  return (
    <section className="desk" aria-label="Chart">
      <div className="desk-head">
        <div>
          <p className="eyebrow">Chart</p>
          <h1>Chart</h1>
        </div>
        {replay ? <span className="chip replay">Replay</span> : <span className="chip neutral">No log</span>}
      </div>
      <div className="masonry">
        <article className="tcard">
          <div className="desk-head"><h2>Price</h2>{demo ? <DemoTag /> : null}</div>
          {demo ? (
            <>
              <p className="hero-value num">NEAR {DEMO_ACCOUNT.nearPrice}</p>
              <Spark points={DEMO_ACCOUNT.holdings[0].spark} up />
            </>
          ) : <p className="hero-value">Unavailable</p>}
          <p className="quiet">{demo ? "Sample NEAR print." : "No price history is recorded."}</p>
        </article>
        <article className="tcard">
          <div className="desk-head"><h2>Position</h2>{demo ? <DemoTag /> : null}</div>
          {demo ? <p className="hero-value num">{usd(DEMO_ACCOUNT.costBasisUsd)}</p> : <p className="hero-value">unknown</p>}
          <p className="quiet">{demo ? `PnL ${usd(DEMO_ACCOUNT.pnlUsd)}` : "Cost basis unknown"}</p>
        </article>
        <article className="tcard span-2">
          <div className="desk-head">
            <h2>PnL</h2>
            {demo ? <DemoTag /> : null}
          </div>
          {demo ? (
            <div className="row">
              {DEMO_RANGES.map((item) => (
                <button key={item} type="button" className="chip-btn" aria-pressed={range === item} onClick={() => setRange(item)}>{item}</button>
              ))}
            </div>
          ) : null}
          {pnl ? <PeleChart source={pnl} points={DEMO_ACCOUNT.pnl[range].map((value, index) => ({ row: index + 1, count: value }))} /> : <p className="empty-well">Unavailable</p>}
        </article>
        <article className="tcard span-2">
          <div className="desk-head">
            <h2>Flow runs</h2>
            <div className="row">
              <label className="chip-btn">
                Import
                <input
                  aria-label="Import DEMO log"
                  type="file"
                  accept=".log,.txt,text/plain"
                  hidden
                  onChange={(event) => {
                    const file = event.target.files?.[0];
                    event.target.value = "";
                    if (file) onImport(file);
                  }}
                />
              </label>
              <button type="button" className="chip-btn" disabled={!replay} onClick={onPlay}>{playing ? "Pause" : "Play"}</button>
              <button type="button" className="chip-btn" disabled={!replay} onClick={onReset}>Reset</button>
              {([0.5, 1, 2] as const).map((value) => (
                <button key={value} type="button" className="chip-btn" disabled={!replay} aria-pressed={speed === value} onClick={() => onSpeed(value)}>{value}×</button>
              ))}
            </div>
          </div>
          {chart ? <PeleChart source={chart.source} points={chart.points} /> : <p>Unavailable</p>}
          {replay ? <p className="quiet">Replay. Bars count stage marks on source rows. Display pacing is not a clock.</p> : null}
        </article>
      </div>
    </section>
  );
}

function PeleChart({ source, points }: { source: string; points: { row: number; count: number }[] }) {
  const host = useRef<HTMLDivElement>(null);
  const [failed, setFailed] = useState(false);
  useEffect(() => {
    const node = host.current;
    if (!node) return;
    setFailed(false);
    const mounted = mount(node, source, { onError: () => setFailed(true) });
    if (mounted.error) setFailed(true);
    return () => mounted.destroy();
  }, [source]);
  if (failed) {
    return (
      <ul className="feed">
        {points.map((point) => (
          <li key={point.row}><span className="num">row {point.row}</span><span>{point.count}</span></li>
        ))}
      </ul>
    );
  }
  return <div className="pele-host" ref={host} />;
}
