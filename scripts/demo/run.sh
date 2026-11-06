#!/usr/bin/env bash
# The NEAR Flows demo in one run (contracts/sandbox/tests/demo.rs): one relay vault
# buys a memecoin on Rhea, a pump.fun coin on Solana, sweeps Meteora LP fees into it,
# and runs an NVDA take-profit ladder on Robinhood Chain. All local: a NEAR sandbox
# node plus mainnet forks (anvil on 9500, surfpool on 9510/9511). Nothing is broadcast
# to a real network.
#
#   scripts/demo/run.sh               build, then run (prints only the narrative)
#   SKIP_BUILD=1 scripts/demo/run.sh  reuse the wasm already built
#   VERBOSE=1 scripts/demo/run.sh     the full test output too
#
# Needs: Rust + cargo-near, Foundry (anvil, cast), surfpool, Node (npx).
# ROBINHOOD_RPC overrides Robinhood Chain's public RPC. Full log: target/demo.log.
set -euo pipefail
cd "$(dirname "$0")/../.."
ROOT=$(pwd)
LOG="$ROOT/contracts/target/demo.log"

need() { command -v "$1" >/dev/null || [ -x "$2" ] || { echo "missing $1: $3" >&2; exit 1; }; }
need anvil "$HOME/.foundry/bin/anvil" "install Foundry (https://getfoundry.sh)"
need surfpool "$HOME/.local/bin/surfpool" "install surfpool (https://surfpool.run)"
need npx "" "install Node.js"
for port in 9500 9510 9511; do
  if lsof -iTCP:$port -sTCP:LISTEN >/dev/null 2>&1; then
    echo "port $port is busy (a fork left running?): free it first" >&2
    exit 1
  fi
done

# The real protocol code (Rhea, wNEAR) the NEAR leg runs, read from testnet once.
[ -s contracts/sandbox/protocols/ref-finance-101.testnet.wasm ] || scripts/fetch-protocols.sh >/dev/null
# The keeper's Solana helper uses the protocols' own SDKs.
[ -d scripts/keeper/node_modules ] || (cd scripts/keeper && npm install --ignore-scripts --silent)
if [ -z "${SKIP_BUILD:-}" ]; then
  echo "building the contracts (make build)..."
  make build >/dev/null 2>&1 || { echo "make build failed: run it by hand to see why" >&2; exit 1; }
fi

echo "starting: NEAR sandbox, Robinhood Chain fork, Solana fork (the first run downloads mainnet state, ~1 min)"
mkdir -p "$(dirname "$LOG")"
start=$(date +%s)
set +e
(cd contracts && cargo test -p sandbox --test demo -- --nocapture 2>&1) | tee "$LOG" | {
  if [ -n "${VERBOSE:-}" ]; then cat; else grep --line-buffered '^DEMO|' | sed -u 's/^DEMO| \{0,1\}//'; fi
}
set -e
if grep -q "test result: ok" "$LOG"; then
  echo
  echo "demo passed in $(( $(date +%s) - start )) s (full log: $LOG)"
else
  echo
  echo "DEMO FAILED. The end of the log ($LOG):" >&2
  grep -A12 "panicked" "$LOG" | head -40 >&2 || tail -40 "$LOG" >&2
  exit 1
fi
