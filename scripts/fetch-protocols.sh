#!/usr/bin/env bash
# Download the real testnet contracts (Rhea DEX, Rhea lending, LiNEAR, wNEAR, and the
# NEAR Intents verifier: the Tidepool sandbox runs the exact intents.near code) for
# the local-node tests. They're public code, fetched straight from the chain.
set -euo pipefail
cd "$(dirname "$0")/../contracts/sandbox/protocols"
for id in wrap.testnet ref-finance-101.testnet linear-protocol.testnet contract.1689937928.burrow.testnet dclv2.ref-dev.testnet verifier.tidepool-sandbox.testnet; do
  [ -s "$id.wasm" ] && continue
  echo "fetching $id"
  curl -s https://rpc.testnet.near.org -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"query\",\"params\":{\"request_type\":\"view_code\",\"finality\":\"final\",\"account_id\":\"$id\"}}" \
    | python3 -c 'import sys,json,base64; sys.stdout.buffer.write(base64.b64decode(json.load(sys.stdin)["result"]["code_base64"]))' > "$id.wasm"
done
ls -la
