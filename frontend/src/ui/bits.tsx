import { useEffect, useState } from "react";

export function usd(value: number): string {
  return value.toLocaleString("en-US", { style: "currency", currency: "USD", maximumFractionDigits: 0 });
}

export function Roll({ value }: { value: number }) {
  const [shown, setShown] = useState(0);
  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      setShown(value);
      return;
    }
    let frame = 0;
    const steps = 16;
    const timer = window.setInterval(() => {
      frame += 1;
      const t = 1 - Math.pow(1 - Math.min(1, frame / steps), 3);
      setShown(Math.round(value * t));
      if (frame >= steps) window.clearInterval(timer);
    }, 28);
    return () => window.clearInterval(timer);
  }, [value]);
  return <span className="hero-value num">{usd(shown)}</span>;
}

export function Spark({ points, up }: { points: number[]; up: boolean }) {
  if (points.length < 2) return null;
  const min = Math.min(...points);
  const max = Math.max(...points);
  const span = max - min || 1;
  const d = points
    .map((point, index) => {
      const x = (index / (points.length - 1)) * 72;
      const y = 22 - ((point - min) / span) * 18;
      return `${index === 0 ? "M" : "L"}${x.toFixed(1)} ${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg className={up ? "spark up" : "spark down"} viewBox="0 0 72 24" aria-hidden="true">
      <path d={d} />
    </svg>
  );
}

export function TokenMark({ symbol }: { symbol: string }) {
  const letter = symbol.replace(/[^A-Za-z]/g, "").slice(0, 1) || symbol.slice(0, 1);
  return <span className="token-mark" aria-hidden="true">{letter}</span>;
}

export function DemoTag() {
  return <span className="chip demo-tag">Demo</span>;
}

export function ChangeChip({ pct }: { pct: number }) {
  const up = pct >= 0;
  const text = `${up ? "+" : ""}${pct.toFixed(1)}%`;
  return <span className={up ? "chip ok" : "chip bad"}>{text}</span>;
}
