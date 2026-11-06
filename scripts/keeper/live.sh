#!/usr/bin/env bash
# The live loop for a deployment: every TICK seconds, post fresh prices to the
# price board (NEAR and NVDA from Chainlink, ETH/USDC/SOL from Pyth's Hermes),
# then run the keeper once (steps whose triggers fired). Vaults refuse prices
# older than their oracle's max age (60 s on the testnet demo), so TICK must be
# well under that.
#
#   SUFFIX=fl06 ./live.sh                  # testnet deployment fl06
#   SUFFIX=fl06 TICK=30 ./live.sh
#
# Secrets come from the environment (or ../../.env): PYTH_API_KEY; the keeper's
# NEAR key from ~/.near-credentials (or KEEPER_PRIVATE_KEY). Nothing is printed.
# Gas: ~0.00004 NEAR per post, two posts per tick: ~0.23 NEAR/day at TICK=30.
set -uo pipefail
cd "$(dirname "$0")"
[ -f ../../.env ] && { set -a; . ../../.env; set +a; }
: "${SUFFIX:?set SUFFIX (the deployment, e.g. fl06)}"
TICK="${TICK:-30}"
export NEAR_NETWORK="${NEAR_NETWORK:-testnet}"
export KEEPER_ACCOUNT_ID="${KEEPER_ACCOUNT_ID:-nf-keeper-$SUFFIX.testnet}"
export FACTORY_ID="${FACTORY_ID:-nf-relay-$SUFFIX.testnet}"
BOARD="${BOARD:-nf-board-$SUFFIX.testnet}"
quiet() { grep -v -E "punycode|trace-deprecation|bigint: Failed" ; }
log() { echo "$(date -u +%H:%M:%S) $*"; }

log "live loop: board $BOARD, factory $FACTORY_ID, keeper $KEEPER_ACCOUNT_ID, every ${TICK}s"
while true; do
  start=$(date +%s)
  npm run -s chainlink-price -- --board "$BOARD" near nvda 2>&1 | quiet | grep -E "^(near|nvda) on|Error|refus" | sed "s/^/$(date -u +%H:%M:%S) /"
  if [ -n "${PYTH_API_KEY:-}" ]; then
    npm run -s pyth -- --board "$BOARD" eth usdc sol 2>&1 | quiet | grep -E " on $BOARD|Error|refus" | sed "s/^/$(date -u +%H:%M:%S) /"
  fi
  npm run -s flows -- --send --once 2>&1 | quiet | grep -E "due|sent|failed|unpaid" | sed "s/^/$(date -u +%H:%M:%S) /"
  wait=$(( TICK - ($(date +%s) - start) ))
  [ "$wait" -gt 0 ] && sleep "$wait"
done
