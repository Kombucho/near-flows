#!/usr/bin/env bash
# The vault's Jupiter swap, executed for real on a local fork of Solana mainnet.
#
# Real pieces: the NEAR testnet vault, the NEAR MPC network's Ed25519 signature,
# Jupiter's mainnet route, mainnet program and pool state (surfpool fetches it on
# demand). No real money: the vault's address is funded on the fork with cheat codes.
#
#   scripts/fork-solana.sh            (needs surfpool, and the keeper's testnet key)
#
# VAULT, KEEPER_ACCOUNT_ID, AMOUNT (USDC base units) can be overridden.
# Public mainnet RPC rate-limits account downloads: on "Failed to fetch accounts"
# run it again (fetched accounts stay cached), or set SURFPOOL_DATASOURCE_RPC_URL.
set -euo pipefail
VAULT=${VAULT:-solvault.nf-keeper-ksleb.testnet}
export KEEPER_ACCOUNT_ID=${KEEPER_ACCOUNT_ID:-nf-keeper-ksleb.testnet}
AMOUNT=${AMOUNT:-100000}
USDC=EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v
WSOL=So11111111111111111111111111111111111111112
TOKEN=TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA
R=http://127.0.0.1:8899
export SOLANA_RPC=$R
cd "$(dirname "$0")/keeper"

rpc() { curl -s -X POST $R -H 'content-type: application/json' -d "$1"; }
if ! rpc '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | grep -q result; then
  # --skip-blockhash-check: Jupiter builds with a mainnet blockhash the fork never produced.
  surfpool start --network mainnet --no-tui --no-studio --no-deploy --skip-blockhash-check -y >/tmp/surfpool.log 2>&1 &
  for _ in $(seq 1 60); do rpc '{"jsonrpc":"2.0","id":1,"method":"getSlot"}' | grep -q result && break; sleep 0.5; done
fi
ADDR=$(curl -s -X POST https://test.rpc.fastnear.com -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"query","params":{"request_type":"call_function","finality":"final","account_id":"'$VAULT'","method_name":"get_sol","args_base64":"e30="}}' \
  | python3 -c "import sys,json;print(json.loads(bytes(json.load(sys.stdin)['result']['result']))['address'])")
echo "vault $VAULT, Solana address $ADDR"
rpc '{"jsonrpc":"2.0","id":1,"method":"surfnet_setAccount","params":["'$ADDR'",{"lamports":50000000}]}' >/dev/null
rpc '{"jsonrpc":"2.0","id":1,"method":"surfnet_setTokenAccount","params":["'$ADDR'","'$USDC'",{"amount":1000000},"'$TOKEN'"]}' >/dev/null
balances() {
  rpc '{"jsonrpc":"2.0","id":1,"method":"getTokenAccountsByOwner","params":["'$ADDR'",{"programId":"'$TOKEN'"},{"encoding":"jsonParsed"}]}' \
    | python3 -c "import sys,json;[print('  ',{'EPjFWd':'USDC','So1111':'wSOL'}.get(a['account']['data']['parsed']['info']['mint'][:6]),a['account']['data']['parsed']['info']['tokenAmount']['uiAmountString']) for a in json.load(sys.stdin)['result']['value']]"
}
echo "before:"; balances
npm run -s jupiter -- --vault "$VAULT" --in $USDC --out $WSOL --amount "$AMOUNT" --broadcast
sleep 3
echo "after:"; balances
