#!/usr/bin/env bash
# Download the real testnet contracts (Rhea DEX, Rhea lending, LiNEAR, wNEAR, and the
# NEAR Intents verifier: the Tidepool sandbox runs the exact intents.near code) for
# the local-node tests, and from mainnet the two codes bridged tokens run today, for
# the hop tests: base.omft.near (the POA token, near/intents contracts/poa/token: the
# EVM chains' omft tokens) and sol.omft.near (Omni Bridge's omni-token 0.4.3, which
# the Solana omft tokens were migrated to).
# They're public code, fetched straight from the chain with read-only view_code.
set -euo pipefail
cd "$(dirname "$0")/../contracts/sandbox/protocols"
fetch() { # rpc account
  [ -s "$2.wasm" ] && return
  echo "fetching $2"
  curl -s "$1" -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"query\",\"params\":{\"request_type\":\"view_code\",\"finality\":\"final\",\"account_id\":\"$2\"}}" \
    | python3 -c 'import sys,json,base64; sys.stdout.buffer.write(base64.b64decode(json.load(sys.stdin)["result"]["code_base64"]))' > "$2.wasm"
}
for id in wrap.testnet ref-finance-101.testnet linear-protocol.testnet contract.1689937928.burrow.testnet dclv2.ref-dev.testnet verifier.tidepool-sandbox.testnet; do
  fetch https://rpc.testnet.near.org "$id"
done
fetch https://rpc.mainnet.near.org sol.omft.near
fetch https://rpc.mainnet.near.org base.omft.near
ls -la
