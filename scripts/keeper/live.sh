#!/usr/bin/env bash
# The live keeper for a deployment: every TICK seconds it runs the steps whose
# triggers fired, and posts prices only when a step needs them (flows --board):
# just before a vault's step goes, fresh prices for the feeds that vault reads and
# their backups from a different publisher (ETH/USDC/SOL from Pyth's Hermes, backed by
# Chainlink on Arbitrum; NEAR from Chainlink on Arbitrum, backed by Optimism; NVDA from
# Chainlink, single-source). The board keeps no average: nothing due, nothing posted, no gas.
#
#   SUFFIX=fl08 ./live.sh                  # testnet deployment fl08
#   SUFFIX=fl08 TICK=30 ./live.sh
#
# Secrets come from the environment (or ../../.env): PYTH_API_KEY; the keeper's
# NEAR key from ~/.near-credentials (or KEEPER_PRIVATE_KEY). Nothing is printed.
# Gas: ~0.00004 NEAR per price post, one per feed source per step sent.
set -uo pipefail
cd "$(dirname "$0")"
[ -f ../../.env ] && { set -a; . ../../.env; set +a; }
: "${SUFFIX:?set SUFFIX (the deployment, e.g. fl08)}"
TICK="${TICK:-30}"
export NEAR_NETWORK="${NEAR_NETWORK:-testnet}"
export KEEPER_ACCOUNT_ID="${KEEPER_ACCOUNT_ID:-nf-keeper-$SUFFIX.testnet}"
export FACTORY_ID="${FACTORY_ID:-nf-relay-$SUFFIX.testnet}"
# The deployment's boards: nf-pyth (pyth_core: Pyth's signed updates, checked on chain) if it
# has one, and nf-board (attested: the Chainlink backups, NEAR, NVDA; ETH/USDC/SOL too without nf-pyth).
PYTH_BOARD="${PYTH_BOARD:-nf-pyth-$SUFFIX.testnet}"
RPC="${NEAR_RPC:-https://test.rpc.fastnear.com}"
has_pyth=$(curl -s "$RPC" -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"query\",\"params\":{\"request_type\":\"view_account\",\"finality\":\"final\",\"account_id\":\"$PYTH_BOARD\"}}" | grep -c '"code_hash"')
BOARD="${BOARD:-$([ "$has_pyth" = 1 ] && echo "$PYTH_BOARD,")nf-board-$SUFFIX.testnet}"

echo "live keeper: board $BOARD, factory $FACTORY_ID, keeper $KEEPER_ACCOUNT_ID, every ${TICK}s, prices on demand"
exec npm run -s flows -- --send --board "$BOARD" --tick "$TICK" 2>&1 | grep --line-buffered -v -E "punycode|trace-deprecation|bigint: Failed"
