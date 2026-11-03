#!/usr/bin/env python3
"""Download real hourly price histories for a fair sample of NEAR launchpad memecoins.

Sample: coins launched on meme.cooking, nearly.trade, justhoot (v1+v2) and tkn
between --from and --to that ever had a trading pool. Random (seeded), stratified
by launchpad, DEAD COINS INCLUDED (no picking winners). Prices are converted to
NEAR (coin USD / NEAR USD per hour) so NEAR's own moves don't pollute the test.

Sources: token list + pools from Intear (prices.intear.tech), hourly OHLCV from
GeckoTerminal (free API, ~30 requests/min, so this takes several minutes).

  python3 scripts/backtest/fetch.py --out contracts/sim-out/backtest
"""
import argparse, csv, json, os, random, sys, time, urllib.request
from datetime import datetime, timezone

GT = "https://api.geckoterminal.com/api/v2/networks/near/pools"
NEAR_USD_POOL = "refv2-17208628f84f5d6ad33f0da3bbbeb27ffcb398eac501a31bd6ad2011e36133a1:wrap.near:100"
LAUNCHPADS = {
    "meme.cooking": (".meme-cooking.near",),
    "nearly.trade": (".nearlytrade.near",),
    "justhoot": (".launchpad.justhoot.near", ".v2-launchpad.justhoot.near"),
    "tkn": (".tkn.near",),
}
QUOTA = {"nearly.trade": 40, "justhoot": 30, "meme.cooking": 20, "tkn": 10}


def get(url):
    for attempt in range(5):
        try:
            req = urllib.request.Request(url, headers={"accept": "application/json", "user-agent": "near-hackathon-backtest"})
            with urllib.request.urlopen(req, timeout=30) as r:
                return json.load(r)
        except Exception as e:  # rate limited or flaky: back off
            time.sleep(5 * (attempt + 1))
            last = e
    raise RuntimeError(f"failed {url}: {last}")


def gecko_pool_id(main_pool):
    """Intear 'REFDCL-a|b|fee' -> 'refv2-a:b:fee'; 'REF-123' -> 'refv1-123'."""
    kind, _, rest = main_pool.partition("-")
    if kind == "REFDCL":
        return "refv2-" + rest.replace("|", ":")
    if kind == "REF":
        return "refv1-" + rest
    return None


def hourly(pool_id, since_s):
    """All hourly closes (USD) for a pool since `since_s`, oldest first."""
    rows, before = {}, None
    while True:
        url = f"{GT}/{pool_id}/ohlcv/hour?limit=1000&currency=usd" + (f"&before_timestamp={before}" if before else "")
        data = get(url).get("data", {}).get("attributes", {}).get("ohlcv_list", [])
        time.sleep(2.2)
        if not data:
            break
        for ts, o, h, l, c, v in data:
            rows[int(ts)] = (float(c), float(v))
        oldest = min(int(r[0]) for r in data)
        if oldest <= since_s or len(data) < 1000:
            break
        before = oldest
    return sorted((t, c, v) for t, (c, v) in rows.items() if t >= since_s)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--from", dest="start", default="2026-09-01")
    ap.add_argument("--to", dest="end", default="2026-10-12")
    ap.add_argument("--seed", type=int, default=42)
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    start = datetime.fromisoformat(a.start).replace(tzinfo=timezone.utc).timestamp()
    end = datetime.fromisoformat(a.end).replace(tzinfo=timezone.utc).timestamp()

    tokens = get("https://prices.intear.tech/tokens")
    pools, never_traded = {}, {}
    for pad, suffixes in LAUNCHPADS.items():
        launched = [v for k, v in tokens.items() if k.endswith(suffixes)
                    and start <= (v.get("created_at_millis") or 0) / 1000 < end]
        never_traded[pad] = sum(1 for v in launched if not v.get("main_pool"))
        pools[pad] = [v for v in launched if v.get("main_pool")]

    rng = random.Random(a.seed)
    sample = []
    for pad, n in QUOTA.items():
        pool = sorted(pools[pad], key=lambda v: v["account_id"])
        sample += [(pad, v) for v in rng.sample(pool, min(n, len(pool)))]
    print(f"sample: {len(sample)} coins; launched-but-never-traded in window: {never_traded}", file=sys.stderr)

    near_usd = {t: c for t, c, _ in hourly(NEAR_USD_POOL, int(start))}
    manifest = []
    for i, (pad, v) in enumerate(sample):
        pid = gecko_pool_id(v["main_pool"])
        if not pid:
            continue
        created = int(v["created_at_millis"] / 1000)
        try:
            rows = hourly(pid, created)
        except RuntimeError as e:
            print(f"  skip {v['account_id']}: {e}", file=sys.stderr)
            continue
        path = os.path.join(a.out, f"coin_{i:03d}.csv")
        with open(path, "w", newline="") as f:
            w = csv.writer(f)
            w.writerow(["ts", "price_near", "volume_usd"])
            for t, c, vol in rows:
                if t in near_usd and near_usd[t] > 0 and c > 0:
                    w.writerow([t, c / near_usd[t], vol])
        manifest.append({"file": os.path.basename(path), "token": v["account_id"], "launchpad": pad,
                         "pool": pid, "created": created, "deleted": bool(v.get("deleted")), "hours": len(rows)})
        print(f"  [{i+1}/{len(sample)}] {v['account_id']}: {len(rows)} hours", file=sys.stderr)
    with open(os.path.join(a.out, "manifest.json"), "w") as f:
        json.dump({"window": [a.start, a.end], "seed": a.seed, "never_traded": never_traded, "coins": manifest}, f, indent=1)


if __name__ == "__main__":
    main()
