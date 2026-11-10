import { useEffect, useState } from "react";
import { EMPTY_BOOK, parseGecko, parseMeteoraPools, parseRefNear, type LiveBook, type PoolRow } from "./quotes";

const SPOT = "https://api.coingecko.com/api/v3/simple/price?ids=near,solana,ethereum,usd-coin&vs_currencies=usd&include_24hr_change=true";
const REF = "https://indexer.ref.finance/list-token-price";
const POOLS = "https://dlmm.datapi.meteora.ag/pools?page_size=6&query=SOL-USDC";

function push(previous: number[], next: number | null | undefined): number[] {
  if (next == null) return [];
  return [...previous, next].slice(-16);
}

export function useLiveBook(): LiveBook {
  const [book, setBook] = useState<LiveBook>(EMPTY_BOOK);
  useEffect(() => {
    let stop = false;
    let fresh = 0;
    async function load() {
      try {
        const response = await fetch(SPOT);
        if (!response.ok) throw new Error(String(response.status));
        const spots = parseGecko(await response.json());
        if (!spots) throw new Error("shape");
        if (stop) return;
        fresh = Date.now();
        setBook((previous) => ({
          status: "live",
          source: "CoinGecko",
          updatedAt: Date.now(),
          ...spots,
          sparks: {
            near: push(previous.sparks.near, spots.near?.usd),
            sol: push(previous.sparks.sol, spots.sol?.usd),
            eth: push(previous.sparks.eth, spots.eth?.usd),
          },
        }));
      } catch {
        try {
          const ref = await fetch(REF);
          if (!ref.ok) throw new Error(String(ref.status));
          const near = parseRefNear(await ref.json());
          if (near == null) throw new Error("ref");
          if (stop) return;
          fresh = Date.now();
          setBook((previous) => ({
            status: "live",
            source: "Ref Finance",
            updatedAt: Date.now(),
            near: { usd: near, changePct: null },
            sol: null,
            eth: null,
            usdc: null,
            sparks: { near: push(previous.sparks.near, near), sol: [], eth: [] },
          }));
        } catch {
          if (!stop && Date.now() - fresh > 5000) setBook(EMPTY_BOOK);
        }
      }
    }
    void load();
    const timer = window.setInterval(() => void load(), 20_000);
    return () => {
      stop = true;
      window.clearInterval(timer);
    };
  }, []);
  return book;
}

export function useMeteoraPools(): { status: "live" | "unavailable"; pools: PoolRow[] } {
  const [state, setState] = useState<{ status: "live" | "unavailable"; pools: PoolRow[] }>({ status: "unavailable", pools: [] });
  useEffect(() => {
    let stop = false;
    async function load() {
      try {
        const response = await fetch(POOLS);
        if (!response.ok) throw new Error(String(response.status));
        const pools = parseMeteoraPools(await response.json());
        if (!pools.length) throw new Error("empty");
        if (!stop) setState({ status: "live", pools });
      } catch {
        if (!stop) setState({ status: "unavailable", pools: [] });
      }
    }
    void load();
    return () => {
      stop = true;
    };
  }, []);
  return state;
}
