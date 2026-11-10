import { useEffect, useRef, useState } from "react";
import { mount, register } from "pele/core";
import xychart from "pele/diagrams/xychart";
import { replayStageChart } from "../model/chart";
import type { ReplayDocument } from "../model/replay";

register(xychart);

export function ChartView({
  replay,
  onImport,
  playing,
  cursor,
  onPlay,
  onReset,
  onSpeed,
  speed,
}: {
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
          <h2>Price</h2>
          <p className="hero-value">Unavailable</p>
          <p className="quiet">No price history is recorded.</p>
        </article>
        <article className="tcard">
          <h2>Position</h2>
          <p className="hero-value">unknown</p>
          <p className="quiet">Cost basis unknown</p>
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
