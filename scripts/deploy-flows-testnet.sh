#!/usr/bin/env bash
# Deploy NEAR Flows to TESTNET: the vault factory (vault code published once as
# a global contract), the Bazaar, and one demo vault with a flow that uses the
# REAL testnet Rhea DEX, LiNEAR and wNEAR. Testnet only: no real value.
#
#   SUFFIX=ksleb ./scripts/deploy-flows-testnet.sh
#
# Expects accounts made with the testnet faucet, keys in ~/.near-credentials/testnet
# (never in this repo): nf-{factory,bazaar,owner,keeper}-$SUFFIX.testnet.
# The factory needs ~35 NEAR (publishing the vault code costs ~30): pool faucet
# accounts into it first (near tokens <a> send-near nf-factory-$SUFFIX.testnet ...).
set -euo pipefail

: "${SUFFIX:?set SUFFIX}"
FACTORY="nf-factory-$SUFFIX.testnet"
BAZAAR="nf-bazaar-$SUFFIX.testnet"
OWNER="nf-owner-$SUFFIX.testnet"
KEEPER="nf-keeper-$SUFFIX.testnet"
VAULT="pedro.$FACTORY"
WRAP="wrap.testnet"
RHEA="ref-finance-101.testnet"
LINEAR="linear-protocol.testnet"
USDT="usdt.itachicara.testnet"            # Rhea pool 1084: wNEAR/USDT, ~87k wNEAR deep
MEME="mev-33.factory.v9.meme-cooking.testnet" # Rhea pool 2131: a meme.cooking testnet coin
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WASM="$ROOT/contracts/target/near"
STEP="${STEP:-all}"   # run one part: build | factory | bazaar | vault | flow | keeper

call() { # call <signer> <contract> <method> <json> <deposit> [gas]
  near contract call-function as-transaction "$2" "$3" json-args "$4" \
    prepaid-gas "${6:-100.0 Tgas}" attached-deposit "$5" \
    sign-as "$1" network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: $1 -> $2.$3"
}
view() { near contract call-function as-read-only "$1" "$2" json-args "${3:-{\}}" network-config testnet now 2>&1 | awk 'f;/printed to stdout/{f=1}'; }
want() { [ "$STEP" = all ] || [ "$STEP" = "$1" ]; }

if want build; then
  echo "== build"
  make -C "$ROOT" build >/dev/null
fi

if want factory; then
  echo "== factory + vault code (global contract, by hash)"
  near contract deploy "$FACTORY" use-file "$WASM/factory/factory.wasm" with-init-call new json-args "{\"owner\":\"$FACTORY\"}" \
    prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: factory deployed"
  near contract call-function as-transaction "$FACTORY" publish file-args "$WASM/vault/vault.wasm" \
    prepaid-gas '300.0 Tgas' attached-deposit '0 NEAR' sign-as "$FACTORY" network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: vault code published, hash $(view "$FACTORY" get_code_hash)"
fi

if want bazaar; then
  echo "== bazaar (treasury: $OWNER, idea review: 15 minutes for the demo; 72h by default)"
  near contract deploy "$BAZAAR" use-file "$WASM/bazaar/bazaar.wasm" with-init-call new \
    json-args "{\"treasury\":\"$OWNER\",\"review_ns\":900000000000}" \
    prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: bazaar deployed"
fi

if want vault; then
  echo "== demo vault $VAULT (owner $OWNER, keeper $KEEPER)"
  call "$OWNER" "$FACTORY" create_vault "{\"name\":\"pedro\",\"keeper\":\"$KEEPER\"}" '1 NEAR' '100.0 Tgas'
  for t in "$WRAP" "$USDT" "$MEME"; do
    call "$OWNER" "$t" storage_deposit "{\"account_id\":\"$VAULT\",\"registration_only\":true}" '0.0125 NEAR'
  done
  call "$OWNER" "$RHEA" storage_deposit "{\"account_id\":\"$VAULT\"}" '0.1 NEAR'
  TRUST=$(python3 -c "import json;print(json.dumps({'add':[
    {'contract':'$WRAP','methods':['near_deposit']},
    {'contract':'$RHEA','methods':['get_return','register_tokens']},
    {'contract':'$LINEAR','methods':['deposit_and_stake']},
    {'contract':'$USDT'},{'contract':'$MEME'}],'remove':[]}))")
  call "$OWNER" "$VAULT" trust "$TRUST" '0 NEAR'
  # The memecoin isn't on Rhea's global whitelist: the vault registers it for itself (a one-off owner action).
  call "$OWNER" "$VAULT" act "{\"target\":\"$RHEA\",\"method\":\"register_tokens\",\"json_args\":\"{\\\"token_ids\\\":[\\\"$MEME\\\"]}\",\"one_yocto\":true}" '0 NEAR' '150.0 Tgas'
  near tokens "$OWNER" send-near "$VAULT" '5 NEAR' network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: 5 NEAR sent to the vault"
fi

if want flow; then
  echo "== demo flow: 1 NEAR -> LiNEAR; 2 NEAR -> wrap -> half USDT, half memecoin; 0.5 NEAR DCA into LiNEAR every 10 min"
  FLOW=$(python3 - "$WRAP" "$RHEA" "$LINEAR" "$USDT" "$MEME" <<'PY'
import json, sys
wrap, rhea, linear, usdt, meme = sys.argv[1:]
E24 = 10**24
def swap(pool, out, min_per_in):
    msg = json.dumps({"force": 0, "actions": [{"pool_id": pool, "token_in": wrap, "token_out": out, "amount_in": "{amount}", "min_amount_out": "{min_out}"}], "skip_unwrap_near": True}, separators=(",", ":"))
    return {"label": f"Swap on Rhea (pool {pool})", "calls": [{"call": "ft", "receiver": rhea, "msg": msg, "tgas": 120}],
            "out": out, "min_out_per_in": str(min_per_in), "price": {"contract": rhea, "method": "get_return",
            "args": json.dumps({"pool_id": pool, "token_in": wrap, "amount_in": str(E24), "token_out": out}), "per": str(E24)}}
stake = {"label": "Stake with LiNEAR", "calls": [{"call": "fn", "contract": linear, "method": "deposit_and_stake", "args": "{}", "deposit": {"kind": "input"}, "tgas": 50}], "out": linear, "min_out_per_in": None, "price": None}
wrapn = {"label": "Wrap NEAR", "calls": [{"call": "fn", "contract": wrap, "method": "near_deposit", "args": "{}", "deposit": {"kind": "input"}, "tgas": 10}], "out": wrap, "min_out_per_in": None, "price": None}
nodes = [
    {"input": "near", "action": stake, "when": {"when": "now"}},
    {"input": "near", "action": wrapn, "when": {"when": "now"}, "next": [{"to": 2, "bps": 5000}, {"to": 3, "bps": 5000}]},
    # USDT has 24 decimals like wNEAR: accept at least 1.0 USDT per wNEAR (pool pays ~1.24).
    {"input": wrap, "action": swap(1084, usdt, 1 * E24), "when": {"when": "now"}},
    # MEV has 18 decimals: the pool pays ~158,000 MEV per wNEAR; accept at least 120,000.
    {"input": wrap, "action": swap(2131, meme, 120_000 * 10**18), "when": {"when": "now"}},
    {"input": "near", "action": dict(stake, label="DCA into LiNEAR"), "when": {"when": "every", "every_ns": 600_000_000_000, "chunk": str(E24 // 10)}},
]
print(json.dumps({"name": "Testnet demo", "nodes": nodes}))
PY
)
  call "$OWNER" "$VAULT" save_flow "$FLOW" '0 NEAR' '100.0 Tgas'
  E24="000000000000000000000000"
  call "$OWNER" "$VAULT" fund "{\"flow_id\":0,\"node\":0,\"amount\":\"1$E24\"}" '0 NEAR'
  call "$OWNER" "$VAULT" fund "{\"flow_id\":0,\"node\":1,\"amount\":\"2$E24\"}" '0 NEAR'
  call "$OWNER" "$VAULT" fund "{\"flow_id\":0,\"node\":4,\"amount\":\"500000000000000000000000\"}" '0 NEAR'
fi

if want keeper; then
  echo "== keeper: three rounds"
  for i in 1 2 3; do
    (cd "$ROOT/scripts/keeper" && FACTORY_ID="$FACTORY" KEEPER_ACCOUNT_ID="$KEEPER" npx tsx src/flows-main.ts --send --once)
  done
fi

echo
echo "Factory: https://testnet.nearblocks.io/address/$FACTORY"
echo "Vault:   https://testnet.nearblocks.io/address/$VAULT"
echo "Bazaar:  https://testnet.nearblocks.io/address/$BAZAAR"
