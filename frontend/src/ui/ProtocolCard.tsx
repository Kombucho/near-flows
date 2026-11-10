import { useEffect, useRef, useState } from "react";
import Tilt from "react-parallax-tilt";
import type { FlowNode } from "../model/flow";
import type { LiveBook } from "../live/quotes";
import type { Stage } from "../model/replay";
import { presentCard, type ThemeId } from "./cardModel";

const NEAR_MARK = "M21.443 0c-.89 0-1.714.46-2.18 1.218l-5.017 7.448a.533.533 0 0 0 .792.7l4.938-4.282a.2.2 0 0 1 .334.151v13.41a.2.2 0 0 1-.354.128L5.03.905A2.56 2.56 0 0 0 3.078 0h-.521A2.557 2.557 0 0 0 0 2.557v18.886a2.557 2.557 0 0 0 4.736 1.338l5.017-7.448a.533.533 0 0 0-.792-.7l-4.938 4.283a.2.2 0 0 1-.333-.152V5.352a.2.2 0 0 1 .354-.128l14.924 17.87c.486.574 1.2.905 1.952.906h.521A2.56 2.56 0 0 0 24 21.445V2.557A2.56 2.56 0 0 0 21.443 0";
const SOL_MARK = "m23.876 18.031l-3.962 4.14a.9.9 0 0 1-.306.21a.9.9 0 0 1-.367.074H.46a.47.47 0 0 1-.252-.073a.45.45 0 0 1-.17-.196a.44.44 0 0 1-.031-.255a.44.44 0 0 1 .117-.23l3.965-4.139a.9.9 0 0 1 .305-.21a.9.9 0 0 1 .366-.075h18.78a.47.47 0 0 1 .252.074a.45.45 0 0 1 .17.196a.44.44 0 0 1 .031.255a.44.44 0 0 1-.117.23m-3.962-8.335a.9.9 0 0 0-.306-.21a.9.9 0 0 0-.367-.075H.46a.47.47 0 0 0-.252.073a.45.45 0 0 0-.17.197a.44.44 0 0 0-.031.254a.44.44 0 0 0 .117.23l3.965 4.14a.9.9 0 0 0 .305.21a.9.9 0 0 0 .366.074h18.78a.47.47 0 0 0 .252-.073a.45.45 0 0 0 .17-.196a.44.44 0 0 0 .031-.255a.44.44 0 0 0-.117-.23zM.46 6.723h18.782a.9.9 0 0 0 .367-.075a.9.9 0 0 0 .306-.21l3.962-4.14a.44.44 0 0 0 .117-.23a.44.44 0 0 0-.032-.254a.45.45 0 0 0-.17-.196a.47.47 0 0 0-.252-.073H4.76a.9.9 0 0 0-.366.074a.9.9 0 0 0-.305.21L.125 5.97a.44.44 0 0 0-.117.23a.44.44 0 0 0 .03.254a.45.45 0 0 0 .17.196a.47.47 0 0 0 .252.074z";
const ETH_MARK = "M11.944 17.97L4.58 13.62L11.943 24l7.37-10.38l-7.372 4.35zM12.056 0L4.69 12.223l7.365 4.354l7.365-4.35z";
const MET_MARK = "M185.89 66.8866C189.203 63.5733 194.335 63.7681 196.457 67.2763C198.753 71.0443 200.486 75.1156 201.72 79.3817C202.413 81.8069 201.611 84.6433 199.598 86.6571L134.696 151.559C130.777 155.479 125.904 158.185 120.729 159.29L115.336 160.437C110.182 161.563 105.266 164.271 101.368 168.169L51.5392 217.998C49.417 209.812 48.4857 204.29 55.134 197.641L185.89 66.8866ZM199.945 94.8475C202.111 92.6821 205.446 93.9384 205.23 96.7968C203.887 114.294 195.55 132.723 180.695 147.579C169.369 159.988 137.405 178.958 114.559 191.67C109.881 194.268 106.069 188.725 109.903 184.891L199.924 94.87L199.945 94.8475ZM170.081 40.6425C172.138 38.5853 175.17 38.1527 177.249 39.5819C181.58 42.5271 185.63 46.7932 187.709 51.9472C188.489 53.9394 187.817 56.3868 186.107 58.0975L116.202 128.001C112.283 131.92 107.41 134.628 102.235 135.732L96.8429 136.88C91.6891 138.006 86.7731 140.713 82.8752 144.61L39.759 187.726C37.6368 179.541 38.6547 172.07 45.3029 165.422L70.466 140.258L170.081 40.6425ZM140.455 28.2157C144.353 24.3178 149.94 22.8017 154.531 24.5341C157.412 25.6385 160.184 26.9815 162.804 28.5624C166.269 30.6414 166.442 35.7086 163.194 38.9569L99.2668 102.884C95.607 106.543 90.6257 108.493 85.8615 108.146C80.8592 107.778 75.5971 109.836 71.7209 113.712L36.7043 148.728C34.582 140.543 37.3324 131.339 43.9806 124.69L140.455 28.2157ZM130.215 21.9989C133.29 21.9558 134.481 25.6162 132.142 27.955L98.9445 61.1522L71.2258 88.872L64.1877 95.9091L53.4025 106.694C51.1503 108.946 47.8155 106.91 49.0715 104.052L59.5529 80.2313C60.1376 78.7804 60.7875 77.3076 61.5021 75.8134L61.5666 75.662C67.6084 62.8636 77.3323 49.285 86.4709 40.1464C101.132 25.4858 114.298 22.1505 130.215 21.9989Z";

function useReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    const media = window.matchMedia("(prefers-reduced-motion: reduce)");
    const apply = () => setReduced(media.matches);
    apply();
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, []);
  return reduced;
}

function Mark({ d, label }: { d: string; label: string }) {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <title>{label}</title>
      <path fill="currentColor" d={d} />
    </svg>
  );
}

function ChainBadge({ chain }: { chain: "near" | "solana" | "robinhood" }) {
  if (chain === "solana") return <span className="energy" title="Chain: Solana"><Mark d={SOL_MARK} label="Solana" /></span>;
  if (chain === "robinhood") return <span className="energy" title="Chain: EVM"><Mark d={ETH_MARK} label="Ethereum" /></span>;
  return <span className="energy" title="Chain: NEAR"><Mark d={NEAR_MARK} label="NEAR" /></span>;
}

function emblemWord(theme: ThemeId, service: string): string {
  if (theme === "uni") return "Uniswap";
  if (theme === "jup") return "Jupiter";
  if (theme === "rhea") return "Rhea";
  if (theme === "intents") return "Intents";
  if (theme === "linear") return "LiNEAR";
  if (theme === "hl") return "HL";
  if (service === "pump") return "pump";
  if (service === "stonk") return "Basket";
  return "Flow";
}

function Emblem({ theme, service, id }: { theme: ThemeId; service: string; id: string }) {
  if (theme === "near") {
    return <div className="emblem" aria-hidden="true"><Mark d={NEAR_MARK} label="NEAR" /></div>;
  }
  if (theme === "met") {
    return (
      <div className="emblem" aria-hidden="true">
        <svg viewBox="0 0 240 240">
          <path d={MET_MARK} fill={`url(#${id})`} />
          <defs>
            <linearGradient id={id} x1="236.796" y1="22.2322" x2="67.9085" y2="217.509" gradientUnits="userSpaceOnUse">
              <stop stopColor="#F5BD00" />
              <stop offset="0.364891" stopColor="#F54B00" />
              <stop offset="1" stopColor="#6E45FF" />
            </linearGradient>
          </defs>
        </svg>
      </div>
    );
  }
  if (theme === "sol") {
    return <div className="emblem" aria-hidden="true"><Mark d={SOL_MARK} label="Solana" /></div>;
  }
  if (theme === "evm") {
    return <div className="emblem" aria-hidden="true"><Mark d={ETH_MARK} label="Ethereum" /></div>;
  }
  return <div className="emblem" aria-hidden="true"><span className="wordmark">{emblemWord(theme, service)}</span></div>;
}

function Art({ theme }: { theme: ThemeId }) {
  if (theme === "met") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <g fill="#F54B00"><rect x="70" y="78" width="9" height="30" /><rect x="81" y="66" width="9" height="42" /><rect x="92" y="54" width="9" height="54" /><rect x="103" y="44" width="9" height="64" /></g>
        <g fill="#F5BD00"><rect x="114" y="38" width="9" height="70" /></g>
        <g fill="#6E45FF"><rect x="125" y="44" width="9" height="64" /><rect x="136" y="54" width="9" height="54" /><rect x="147" y="66" width="9" height="42" /><rect x="158" y="78" width="9" height="30" /></g>
        <line x1="118.5" y1="26" x2="118.5" y2="112" stroke="#f9f9fb" strokeOpacity="0.6" strokeDasharray="3 3" />
        <g strokeWidth="7" strokeLinecap="round" opacity="0.18">
          <line x1="180" y1="10" x2="234" y2="64" stroke="#F5BD00" />
          <line x1="196" y1="8" x2="240" y2="52" stroke="#F54B00" />
          <line x1="212" y1="6" x2="246" y2="40" stroke="#6E45FF" />
        </g>
      </svg>
    );
  }
  if (theme === "uni") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <rect x="96" y="14" width="56" height="100" fill="#F50DB4" fillOpacity="0.14" />
        <path d="M40 18 C 70 86, 120 100, 210 106" fill="none" stroke="#F50DB4" strokeWidth="2.5" strokeLinecap="round" />
        <circle cx="124" cy="92" r="5" fill="#F50DB4" />
        <circle cx="124" cy="92" r="10" fill="none" stroke="#F50DB4" strokeOpacity="0.4" />
        <text x="98" y="24" fill="#F50DB4" fillOpacity="0.7" fontFamily="Inter, sans-serif" fontWeight="700" fontSize="8">tick range</text>
      </svg>
    );
  }
  if (theme === "near") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <g fill="none" stroke="currentColor" strokeOpacity="0.35" strokeWidth="1.2">
          <path d="M40 92 C 80 20, 150 20, 190 92" />
          <path d="M58 92 C 92 40, 138 40, 172 92" />
        </g>
        <circle cx="96" cy="70" r="13" fill="currentColor" fillOpacity="0.85" />
        <circle cx="138" cy="70" r="13" fill="none" stroke="currentColor" strokeWidth="2.5" />
        <text x="117" y="74" textAnchor="middle" fontFamily="Inter, sans-serif" fontWeight="700" fontSize="10" fill="currentColor" fillOpacity="0.55">⇄</text>
      </svg>
    );
  }
  if (theme === "jup") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <ellipse cx="118" cy="64" rx="54" ry="22" fill="none" stroke="#C7F284" strokeWidth="1.4" />
        <ellipse cx="118" cy="64" rx="34" ry="14" fill="none" stroke="#00BEF0" strokeWidth="1.4" />
        <circle cx="118" cy="64" r="6" fill="#C7F284" />
        <circle cx="168" cy="58" r="3" fill="#00BEF0" />
      </svg>
    );
  }
  if (theme === "rhea" || theme === "linear") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <g fill="none" stroke="currentColor" strokeOpacity="0.45">
          <ellipse cx="117" cy="78" rx="28" ry="8" />
          <ellipse cx="117" cy="78" rx="48" ry="14" />
          <ellipse cx="117" cy="78" rx="70" ry="20" />
        </g>
      </svg>
    );
  }
  if (theme === "hl") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <path d="M16 70 C 46 30, 70 100, 100 62 S 150 30, 214 68" fill="none" stroke="#97FCE4" strokeWidth="3" strokeLinecap="round" />
      </svg>
    );
  }
  if (theme === "sol") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <g fill="#14F195"><rect x="78" y="78" width="78" height="8" rx="2" /><rect x="78" y="60" width="78" height="8" rx="2" opacity="0.7" /></g>
        <rect x="78" y="42" width="78" height="8" rx="2" fill="#9945FF" />
      </svg>
    );
  }
  if (theme === "evm") {
    return (
      <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
        <path d="M117 18 L156 62 L117 108 L78 62 Z" fill="none" stroke="#8A92B2" strokeWidth="1.6" />
        <path d="M117 18 L117 108 M78 62 L156 62" stroke="#62688F" strokeWidth="1.2" />
      </svg>
    );
  }
  return (
    <svg viewBox="0 0 234 122" width="100%" height="100%" aria-hidden="true">
      <path d="M36 86 C 80 40, 150 40, 198 78" fill="none" stroke="currentColor" strokeOpacity="0.45" strokeWidth="1.6" strokeDasharray="4 4" />
      <circle cx="70" cy="68" r="5" fill="currentColor" fillOpacity="0.8" />
      <circle cx="164" cy="62" r="5" fill="none" stroke="currentColor" />
    </svg>
  );
}

function Rarity({ demo }: { demo: boolean }) {
  if (demo) {
    return (
      <svg className="rar" viewBox="0 0 10 10" aria-label="Demo card">
        <path d="M5 0l5 5-5 5-5-5z" fill="#ff9f0a" />
      </svg>
    );
  }
  return (
    <svg className="rar" viewBox="0 0 10 10" aria-label="Live card">
      <circle cx="5" cy="5" r="4" fill="currentColor" />
    </svg>
  );
}

export function ProtocolCard({
  node,
  demo,
  book,
  now,
  evidenced,
  usePosition,
  compact,
  dragging,
  pulse,
  selected,
  onOpen,
}: {
  node: FlowNode;
  demo: boolean;
  book: LiveBook;
  now: number;
  evidenced: Stage[];
  usePosition: boolean;
  compact: boolean;
  dragging: boolean;
  pulse: boolean;
  selected: boolean;
  onOpen: (id: string) => void;
}) {
  const reduced = useReducedMotion();
  const cardRef = useRef<HTMLElement>(null);
  const face = presentCard({ node, demo, book, now, evidenced, usePosition });
  const tilt = !compact && !reduced && !dragging;
  const damageClass = face.damageUnknown ? "unk" : face.damageUp == null ? "" : face.damageUp ? "pos" : "neg";

  function setHolo(amount: number, x?: number, y?: number) {
    const el = cardRef.current;
    if (!el) return;
    if (x != null) el.style.setProperty("--mx", `${x}%`);
    if (y != null) el.style.setProperty("--my", `${y}%`);
    el.style.setProperty("--holo", String(amount));
  }

  return (
    <Tilt
      className="pcard-tilt"
      tiltEnable={tilt}
      tiltMaxAngleX={4}
      tiltMaxAngleY={6}
      perspective={900}
      scale={1}
      transitionSpeed={500}
      transitionEasing="cubic-bezier(.2, 0, 0, 1)"
      glareEnable={false}
      gyroscope={false}
      trackOnWindow={false}
      reset
      onMove={(values) => setHolo(1, values.tiltAngleYPercentage, values.tiltAngleXPercentage)}
      onLeave={() => setHolo(0)}
    >
      <article
        ref={cardRef}
        className={compact ? `pcard compact ${face.theme}${selected ? " is-selected" : ""}` : `pcard ${face.theme}${selected ? " is-selected" : ""}`}
        tabIndex={0}
        data-pulse={pulse ? "1" : "0"}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            onOpen(node.id);
          }
        }}
      >
        <div className="face">
          <div className="namebar">
            <span className="nm">
              <span className="stage">{face.stage}</span>
              <span className="name">{face.name}</span>
            </span>
            <span className="hp">
              <small>VAL</small>
              {face.valueUnknown ? <span className="unk">unknown</span> : face.value}
            </span>
            <ChainBadge chain={face.chain} />
          </div>
          <div className="art">
            <Art theme={face.themeId} />
            <Emblem theme={face.themeId} service={node.service} id={`met-${node.id}`} />
            {face.stamp ? <span className="stamp">{face.stamp}</span> : null}
            <div className="torn" />
          </div>
          <div className="typeline">{face.typeLine}</div>
          <div className="attack">
            <div className="cost">{face.costs === 2 ? <><i /><i className="b" /></> : <i />}</div>
            <div className="atk-name">{face.verb} <span>· {face.pair}</span></div>
            <div className={face.stale ? `dmg stale ${damageClass}` : `dmg ${damageClass}`}>
              {face.damageUnknown ? <span className="unk">{face.damage}</span> : face.damage}
              <small>{face.damageLabel}</small>
            </div>
          </div>
          <div className="flavor">{face.flavor}</div>
          <div className="stats">
            <div><label>Entry</label><b className={face.entryUnknown ? "unk" : undefined}>{face.entry}</b></div>
            <div><label>Amount</label><b className={face.amountUnknown ? "unk" : undefined}>{face.amount}</b></div>
            <div>
              <label>Change</label>
              <b className={face.changeUnknown ? "unk" : face.changeUp ? "pos" : face.changeUp === false ? "neg" : undefined}>{face.change}</b>
            </div>
          </div>
          <div className="src">
            <span>{face.source}</span>
            <span>{face.reporter}</span>
          </div>
          {face.attribution ? <p className="attr">{face.attribution}</p> : null}
          <div className="foot">
            {face.stages.length === 0 ? <span>No stages yet</span> : face.stages.map((stage) => (
              <span key={stage.name} className={stage.failed ? "pip bad" : "pip on"}>{stage.name}</span>
            ))}
            <span className="set">{face.serial} <Rarity demo={face.demo} /></span>
          </div>
        </div>
      </article>
    </Tilt>
  );
}
