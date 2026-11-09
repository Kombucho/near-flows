#!/usr/bin/env bash
# Deploy the SPLIT design to TESTNET: one shared brain, the relay (vault body)
# published once as a global contract by a relay factory, a price board, and a
# demo relay vault with oracle feeds and a flow on the REAL testnet Rhea.
# Testnet only: no real value. See scripts/DEPLOY-RELAY.md for costs and rollback.
#
#   SUFFIX=abcde ./scripts/deploy-relay-testnet.sh                # everything
#   SUFFIX=abcde STEP=costs ./scripts/deploy-relay-testnet.sh     # the funding table only
#   SUFFIX=abcde STEP=vault,flow ./scripts/deploy-relay-testnet.sh
#   SUFFIX=abcde DRY=1 ./scripts/deploy-relay-testnet.sh          # print every command, send nothing
#   SUFFIX=abcde STEP=oracle ./scripts/deploy-relay-testnet.sh    # re-point an existing vault's feeds to the board
#
#   SUFFIX=abcde PYTH_SOURCE=attested ./scripts/deploy-relay-testnet.sh   # no pyth_core board: the keeper vouches for Pyth's prices
#
# STEP (comma list): build accounts brain factory publish priceboard pythboard vault oracle flow check costs (default all).
# PYTH_SOURCE: core (default) deploys a second board, nf-pyth-$SUFFIX, that checks Pyth's signed
# updates itself (Pyth's signer set, pinned here); attested puts ETH/USDC/SOL on the keeper-attested board.
# DRY=1 prints the exact near-cli commands and sends nothing (check still READS testnet).
# PLAN=<file> also writes each command as a JSON line: contracts/sandbox/tests/deploy_sequence.rs
# replays that file in a local sandbox, so the args here can't drift from the contracts.
#
# Accounts (faucet, keys in ~/.near-credentials/testnet, never in this repo):
#   nf-brain-$SUFFIX  nf-relay-$SUFFIX (the factory)  nf-board-$SUFFIX  nf-owner-$SUFFIX  nf-keeper-$SUFFIX
#   and nf-pyth-$SUFFIX (the pyth_core board) unless PYTH_SOURCE=attested
set -euo pipefail

: "${SUFFIX:?set SUFFIX (a new one: account names must be free)}"
BRAIN="nf-brain-$SUFFIX.testnet"
FACTORY="nf-relay-$SUFFIX.testnet"
BOARD="nf-board-$SUFFIX.testnet"
PYTH_SOURCE="${PYTH_SOURCE:-core}"
case "$PYTH_SOURCE" in core | attested) ;; *) echo "PYTH_SOURCE: core or attested"; exit 1 ;; esac
PYTH_BOARD="nf-pyth-$SUFFIX.testnet"
# Where the vault reads ETH/USDC/SOL (Pyth's): the pyth_core board, or the attested one.
PBOARD="$([ "$PYTH_SOURCE" = core ] && echo "$PYTH_BOARD" || echo "$BOARD")"
OWNER="${OWNER_ID:-nf-owner-$SUFFIX.testnet}"
KEEPER="${KEEPER_ID:-nf-keeper-$SUFFIX.testnet}"
# Fees (brain-enforced, caps fixed at deploy): protocol 0.1% of what a step uses (cap 0.3%),
# 0.01 NEAR per cross-chain signature (cap 0.05). The owner is treasury and fee admin on testnet.
TREASURY="${TREASURY_ID:-$OWNER}"
FEES="{\"treasury\":\"$TREASURY\",\"admin\":\"$OWNER\",\"cap_bps\":30,\"bps\":10,\"per_signature\":\"10000000000000000000000\",\"per_signature_cap\":\"50000000000000000000000\"}"
VAULT_NAME="${VAULT_NAME:-demo}"
VAULT="$VAULT_NAME.$FACTORY"
# Real testnet contracts.
WRAP="wrap.testnet"
RHEA="ref-finance-101.testnet"
LINEAR="linear-protocol.testnet"
USDT="usdt.itachicara.testnet"                # Rhea pool 1084: wNEAR/USDT
MEME="mev-33.factory.v9.meme-cooking.testnet" # Rhea pool 2131: a meme.cooking testnet coin
RPC="${NEAR_RPC:-https://rpc.testnet.near.org}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WASM="$ROOT/contracts/target/near"
STEP="${STEP:-all}"
DRY="${DRY:-0}"
PLAN="${PLAN:-}"
FAUCET="${FAUCET:-10}"              # NEAR the faucet gives a new account
VAULT_DEPOSIT="${VAULT_DEPOSIT:-1}" # NEAR attached to create_vault (min 0.1): the vault's own float
VAULT_FUND="${VAULT_FUND:-5}"       # NEAR the owner sends the vault for the flow
TOKEN_STORAGE="0.0125"              # per token; registration_only refunds the excess (min is 0.00125)
RHEA_STORAGE="0.1"                  # Rhea's min is 0.00102, but each registered token takes more
GAS_X="300.0 Tgas"                  # a relay request (mainnet cap): brain.handle gets 40 (relay v2), the vault's cb the rest
E24="000000000000000000000000"

# Every feed is read from OUR price boards. WHY not Pyth's NEAR contract: on 2026-11-06
# pyth-oracle.testnet refused every Hermes update ("Source for attestation is not allowed":
# Pyth's Aug 2026 Core upgrade moved the data source and the NEAR receiver wasn't updated).
# WHY not Pyth at all for NEAR and NVDA: our Pyth key isn't entitled to them, so those come
# from Chainlink on other chains.
#   ETH, USDC, SOL: Pyth's own ids. PYTH_SOURCE=core: on nf-pyth, which checks Pyth's signed
#   update itself (the keeper only carries it: npm run pyth -- --board nf-pyth...). attested:
#   on nf-board, the keeper relays and signs Hermes's prices (keeper-vouched).
#   Everything else is on nf-board, attested by the keeper (k=1).
#   NEAR, NVDA and every backup: Chainlink feeds, id = Wormhole chain (2 B) + 10 zero bytes +
#   proxy address (npm run chainlink-price -- --board). The same ids work on an Evm board.
# Every feed's backup is a DIFFERENT publisher, read at step time (the board keeps no average):
# Pyth's ETH/USDC/SOL by Chainlink on Arbitrum, Chainlink NEAR on Arbitrum by Chainlink NEAR on
# Optimism, Chainlink's RHNVDA by the 30-min TWAP of the NVDA/USDG 0.05% Uniswap v3 pool on
# Robinhood Chain (same id scheme, the pool's address: npm run twap-price -- --board).
USDC_ID="eaa020c61cc479712813461ce153894a96a6c00b21ed0cfc2798d1f9a9e9c94a"
ETH_ID="ff61491a931112ddf1bd8147cd1b641375f79f5825126d665480874634fd0ace"
SOL_ID="ef0d8b6fda2ceba41da15d4095d1da392a0d2f8ed0c6c7bc0f4cfac8c280b56d"
NEAR_ID="001700000000000000000000bf5c3fb2633e924598a46b9d07a174a9dbcf57c0" # Chainlink NEAR/USD, Arbitrum (23)
NVDA_ID="004800000000000000000000379ec4f7c378f34a1b47e4f3cbebcbac3e8e9f15" # Chainlink RHNVDA/USD, Robinhood Chain (72)
NEAR_OP_ID="001800000000000000000000ca6fa4b8cb365c02cd3ba70544efffe78f63ac82" # Chainlink NEAR/USD, Optimism (24): NEAR's backup
ETH_ARB_ID="001700000000000000000000639fe6ab55c921f74e7fac1ee960c0b6293ba612"  # Chainlink ETH/USD, Arbitrum (23): ETH's backup
SOL_ARB_ID="00170000000000000000000024cea4b8ce57cda5058b924b9b9987992450590c"  # Chainlink SOL/USD, Arbitrum (23): SOL's backup
USDC_ARB_ID="00170000000000000000000050834f3163758fcc1df9973b6e91f0f0f0434ad3" # Chainlink USDC/USD, Arbitrum (23): USDC's backup
NVDA_TWAP_ID="004800000000000000000000d4eb21209c4d6093f80b5b84f5c45cc093ea14a3" # NVDA/USDG Uniswap v3 TWAP, Robinhood Chain (72): NVDA's backup
# Pyth's signer set since the Aug 2026 Core upgrade: guardian set 1, its 5 routers, 3 of 5 sign
# (getGuardianSet(1) on Pyth's wormhole receivers on Arbitrum and Base, identical; checked against
# real updates: contracts/priceboard/tests-data/pyth_core.json). Later sets: anyone posts the
# upgrade VAA this set signs (upgrade_guardians). Roots come from Pythnet (chain 26), this emitter.
PYTH_SET='["41534bb176e461a3fb30479400f210549ecce638","6502987b62f21cab7eb5ccd8f0173084b60d5b41","44a3e8f6a382412cf6bb90a3f8106e68977476c9","13edc776d3063549fdb0702af182edc905a539d4","3af088854bb768065f4929e5bbdfa4cbb04c9af9"]'
PYTHNET_EMITTER="507974686e6574507974686e6574507974686e6574507974686e657450797468"

want() { [ "$STEP" = all ] && [ "$1" != costs ] && return 0; [[ ",$STEP," == *",$1,"* ]]; }
size() { wc -c <"$WASM/$1/$1.wasm" | tr -d ' '; }

# One JSON line per command for the sandbox replay: emit k v k v ...
emit() { [ -z "$PLAN" ] || python3 -c 'import json,sys;a=sys.argv[1:];print(json.dumps(dict(zip(a[::2],a[1::2]))))' "$@" >>"$PLAN"; }
# Run a near-cli command, or (DRY) print it exactly, shell-quoted.
run() {
  if [ "$DRY" = 1 ]; then printf '  $'; printf ' %q' "$@"; echo; else "$@" >/dev/null; fi
}
mkacct() { # mkacct <account>
  emit op create account "$1" near "$FAUCET"
  if [ "$DRY" != 1 ] && [ -f "$HOME/.near-credentials/testnet/$1.json" ]; then echo "  have: $1"; return; fi
  run near account create-account sponsor-by-faucet-service "$1" autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
  echo "  ok: $1"
}
deploy() { # deploy <account> <wasm name> <init json>
  emit op deploy account "$1" wasm "$2" method new args "$3" gas "100.0 Tgas"
  run near contract deploy "$1" use-file "$WASM/$2/$2.wasm" with-init-call new json-args "$3" \
    prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' network-config testnet sign-with-legacy-keychain send
  echo "  ok: $2 -> $1"
}
call() { # call <signer> <contract> <method> <json> <deposit> [gas]
  emit op call signer "$1" contract "$2" method "$3" args "$4" deposit "$5" gas "${6:-100.0 Tgas}"
  run near contract call-function as-transaction "$2" "$3" json-args "$4" \
    prepaid-gas "${6:-100.0 Tgas}" attached-deposit "$5" sign-as "$1" network-config testnet sign-with-legacy-keychain send
  echo "  ok: $1 -> $2.$3"
}
x() { # x <signer> <request name> <args json>: a relay vault request, {"<name>": <args>}
  call "$1" "$VAULT" x "{\"$2\":$3}" '0 NEAR' "$GAS_X"
}
send() { # send <from> <to> <NEAR>
  emit op send signer "$1" to "$2" deposit "$3 NEAR"
  run near tokens "$1" send-near "$2" "$3 NEAR" network-config testnet sign-with-legacy-keychain send
  echo "  ok: $3 NEAR $1 -> $2"
}
# Read-only RPC: an account's balance, or a view (JSON out). Never signs anything.
rpc() {
  python3 - "$RPC" "$@" <<'PY'
import base64, json, sys, urllib.request
rpc, kind, acc = sys.argv[1:4]
p = {"request_type": "view_account", "finality": "final", "account_id": acc}
if kind == "view":
    p = {"request_type": "call_function", "finality": "final", "account_id": acc, "method_name": sys.argv[4],
         "args_base64": base64.b64encode((sys.argv[5] if len(sys.argv) > 5 else "{}").encode()).decode()}
req = urllib.request.Request(rpc, json.dumps({"jsonrpc": "2.0", "id": 1, "method": "query", "params": p}).encode(), {"content-type": "application/json"})
try:
    r = json.load(urllib.request.urlopen(req, timeout=20))
except Exception as e:
    print(f"(unreachable: {e})"); sys.exit(0)
res = r.get("result") or {}
if "error" in r or "error" in res:
    print("(" + str(r.get("error", {}).get("cause", {}).get("name") or res.get("error", "error"))[:100] + ")")
elif kind == "view":
    print(bytes(res["result"]).decode()[:300])
else:  # free NEAR (balance minus what storage locks), storage NEAR, bytes
    amt, used = int(res["amount"]), res["storage_usage"] * 10**19
    print(f"{(amt - used) / 1e24:.4f} {used / 1e24:.4f} {res['storage_usage']}")
PY
}
# Live runs stop before spending if an account can't cover its stage.
need() { # need <account> <NEAR>
  if [ "$DRY" = 1 ]; then echo "  needs: $2 NEAR free on $1"; return; fi
  local free; free=$(rpc account "$1" | awk '{print $1}')
  python3 -c "import sys; sys.exit(0 if float('$free') >= $2 else 1)" 2>/dev/null || {
    echo "  STOP: $1 has ${free:-?} NEAR free, this stage needs $2. Top it up first, e.g.:"
    echo "    near tokens <funded account> send-near $1 '$2 NEAR' network-config testnet sign-with-legacy-keychain send"
    exit 1
  }
}

# The funding table, from the wasm sizes. Storage: 1e19 yocto/byte (0.01 NEAR per KB);
# a GLOBAL contract costs 10x that (0.1 NEAR per KB) and is burned, not staked.
costs() {
  [ -f "$WASM/brain/brain.wasm" ] || { echo "  (no wasm yet: run STEP=build first)"; exit 1; }
  python3 - "$(size brain)" "$(size relay)" "$(size factory_relay)" "$(size priceboard)" \
    "$FAUCET" "$VAULT_DEPOSIT" "$VAULT_FUND" "$TOKEN_STORAGE" "$RHEA_STORAGE" "$PYTH_SOURCE" <<'PY'
import sys
brain, relay, fac, board = (int(a) for a in sys.argv[1:5])
faucet, vdep, vfund, tok, rhea = (float(a) for a in sys.argv[5:10])
core = sys.argv[10] == "core"
kb = lambda b, per_kb: b / 1000 * per_kb
publish = kb(relay, 0.1)
# Spend (or lock) per account; gas at testnet's 1e-4 NEAR/Tgas is ~0.005-0.03 NEAR per tx.
rows = [
    # stage, account, spends/locks, what, refundable
    ("brain",      "brain",   kb(brain, 0.01) + 0.002 + 0.05, f"brain code {brain:,} B ({kb(brain, .01):.3f}) + state + gas", "yes: delete the account"),
    ("factory",    "factory", kb(fac, 0.01) + 0.002 + 0.03,   f"factory code {fac:,} B ({kb(fac, .01):.3f}) + gas",            "yes: delete the account"),
    ("publish",    "factory", publish + 0.05,                  f"relay GLOBAL code {relay:,} B ({publish:.4f}, burned) + gas",  "NO: burned"),
    ("priceboard", "board",   kb(board, 0.01) + 0.002 + 0.03, f"board code {board:,} B ({kb(board, .01):.3f}) + gas",         "yes: delete the account"),
    ("vault",      "owner",   vdep + 3 * tok + rhea + vfund + 0.2, f"vault {vdep} + token storage {3*tok:.4f} + Rhea {rhea} + fund {vfund} + gas", "vault NEAR: withdraw (0.05 reserve stays)"),
    ("flow",       "owner",   0.05,                            "gas only (funds come from the vault)",                           "-"),
]
if core:  # the same code again, on nf-pyth (pyth_core)
    rows.insert(4, ("pythboard", "pyth", kb(board, 0.01) + 0.003 + 0.03, f"board code {board:,} B ({kb(board, .01):.3f}) + 3 prices + gas", "yes: delete the account"))
print(f"  {'stage':<11}{'pays':<9}{'NEAR':>8}  what  [refundable?]")
per = {}
for st, who, n, what, back in rows:
    per[who] = per.get(who, 0) + n
    print(f"  {st:<11}{who:<9}{n:>8.3f}  {what}  [{back}]")
print(f"  {'(later)':<11}{'keeper':<9}{0.1:>8.3f}  keeper steps, Pyth pushes (gas)")
per["keeper"] = 0.1
print(f"\n  per account (faucet gives ~{faucet:g} NEAR each):")
for who in ("brain", "factory", "board", *(["pyth"] if core else []), "owner", "keeper"):
    flag = "ok" if per[who] <= faucet - 0.1 else f"TOP UP {per[who] - faucet + 0.5:.1f} NEAR"
    print(f"    {who:<8}{per[who]:>7.3f}  {flag}")
print(f"  total {sum(per.values()):.2f} NEAR; burned for good: {publish:.4f} (relay code) + gas")
print(f"PUBLISH_DEPOSIT={publish + 0.1:.3f}")
PY
}

echo "== NEAR Flows relay deploy, testnet, SUFFIX=$SUFFIX$([ "$DRY" = 1 ] && echo ' (DRY: nothing is sent)')"
echo "   brain $BRAIN | factory $FACTORY | board $BOARD$([ "$PYTH_SOURCE" = core ] && echo " | pyth board $PYTH_BOARD") | owner $OWNER | keeper $KEEPER | vault $VAULT"
echo "   Pyth's ETH/USDC/SOL: $([ "$PYTH_SOURCE" = core ] && echo "pyth_core on $PYTH_BOARD (Pyth's signers checked on chain)" || echo "attested on $BOARD (keeper-vouched)")"

if want build; then
  echo "== build"
  if [ "$DRY" = 1 ]; then echo "  \$ make -C $ROOT build"; else make -C "$ROOT" build >/dev/null; fi
fi

PUBLISH_DEPOSIT=1.1
if [ -f "$WASM/brain/brain.wasm" ]; then
  echo "== funding needed (before anything is spent)"
  T=$(costs); echo "$T" | grep -v '^PUBLISH_DEPOSIT='
  PUBLISH_DEPOSIT=$(echo "$T" | sed -n 's/^PUBLISH_DEPOSIT=//p')
elif want costs; then costs; fi

if want accounts; then
  echo "== accounts (faucet; one call each, keys saved to ~/.near-credentials/testnet)"
  for a in "$BRAIN" "$FACTORY" "$BOARD" $([ "$PYTH_SOURCE" = core ] && echo "$PYTH_BOARD") "$OWNER" "$KEEPER"; do mkacct "$a"; done
fi

if want brain; then
  echo "== brain: rules + state of every relay vault; only vaults the factory makes"
  need "$BRAIN" "$(python3 -c "print(round($(size brain)/1e5 + 0.06, 2))")"
  deploy "$BRAIN" brain "{\"factory\":\"$FACTORY\",\"fees\":$FEES}"
fi

if want factory; then
  echo "== relay factory, bound to the brain for good"
  need "$FACTORY" "$(python3 -c "print(round($(size factory_relay)/1e5 + $PUBLISH_DEPOSIT + 0.05, 2))")"
  deploy "$FACTORY" factory_relay "{\"owner\":\"$FACTORY\",\"brain\":\"$BRAIN\"}"
fi

if want publish; then
  # WHY a function call and not `near contract deploy-as-global`: the factory itself must
  # publish, so it records the code hash its vaults are made from. The global deploy is
  # charged to the factory's balance; the deposit only guarantees the factory holds it.
  echo "== publish the relay as a global contract (by hash), $PUBLISH_DEPOSIT NEAR attached"
  emit op call signer "$FACTORY" contract "$FACTORY" method publish file "relay" deposit "$PUBLISH_DEPOSIT NEAR" gas "300.0 Tgas"
  run near contract call-function as-transaction "$FACTORY" publish file-args "$WASM/relay/relay.wasm" \
    prepaid-gas '300.0 Tgas' attached-deposit "$PUBLISH_DEPOSIT NEAR" sign-as "$FACTORY" network-config testnet sign-with-legacy-keychain send
  echo "  ok: relay published$([ "$DRY" = 1 ] || echo ", hash $(rpc view "$FACTORY" get_code_hash)")"
fi

if want priceboard; then
  # Attested by the keeper's own key (k=1, the jupiter-price script's default signer).
  # Testnet only: one attester means trusting the keeper. Add attesters with set_attesters.
  echo "== price board (attested, attester = $KEEPER's key, k=1)"
  KEY="${ATTESTER_KEY:-$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['public_key'])" "$HOME/.near-credentials/testnet/$KEEPER.json" 2>/dev/null || true)}"
  if [ -z "$KEY" ]; then
    [ "$DRY" = 1 ] || { echo "  no key for $KEEPER: run STEP=accounts first"; exit 1; }
    KEY="ed25519:11111111111111111111111111111111"; echo "  (DRY: $KEEPER has no key yet, placeholder used)"
  fi
  need "$BOARD" "$(python3 -c "print(round($(size priceboard)/1e5 + 0.04, 2))")"
  deploy "$BOARD" priceboard "{\"owner\":\"$OWNER\",\"source\":{\"kind\":\"attested\",\"attesters\":[\"$KEY\"],\"k\":1,\"max_skew_s\":60}}"
fi

if want pythboard && [ "$PYTH_SOURCE" = core ]; then
  # Pyth's ETH, USDC, SOL, checked on chain: anyone posts Hermes's update as it is (post_update),
  # the board checks 3 of Pyth's 5 signers, the Pythnet emitter and each price's Merkle proof.
  # The owner (nf-owner) can only change which feeds are kept; the signer set moves only by
  # an upgrade VAA the current set signed. Prices older than 60 s when posted are refused.
  echo "== pyth board (pyth_core: Pyth's signer set 1, 3 of 5; ETH, USDC, SOL)"
  need "$PYTH_BOARD" "$(python3 -c "print(round($(size priceboard)/1e5 + 0.04, 2))")"
  deploy "$PYTH_BOARD" priceboard "{\"owner\":\"$OWNER\",\"source\":{\"kind\":\"pyth_core\",\"guardian_set\":1,\"guardians\":$PYTH_SET,\"emitter_chain\":26,\"emitter\":\"$PYTHNET_EMITTER\",\"feeds\":[\"$ETH_ID\",\"$USDC_ID\",\"$SOL_ID\"],\"max_age_s\":60}}"
fi

if want vault; then
  echo "== demo vault $VAULT (owner $OWNER, keeper $KEEPER)"
  need "$OWNER" "$(python3 -c "print(round($VAULT_DEPOSIT + 3*$TOKEN_STORAGE + $RHEA_STORAGE + $VAULT_FUND + 0.2, 3))")"
  call "$OWNER" "$FACTORY" create_vault "{\"name\":\"$VAULT_NAME\",\"keeper\":\"$KEEPER\"}" "$VAULT_DEPOSIT NEAR" '300.0 Tgas'
  for t in "$WRAP" "$USDT" "$MEME"; do
    call "$OWNER" "$t" storage_deposit "{\"account_id\":\"$VAULT\",\"registration_only\":true}" "$TOKEN_STORAGE NEAR"
  done
  call "$OWNER" "$RHEA" storage_deposit "{\"account_id\":\"$VAULT\"}" "$RHEA_STORAGE NEAR"
  x "$OWNER" trust "{\"add\":[{\"contract\":\"$WRAP\",\"methods\":[\"near_deposit\"]},{\"contract\":\"$RHEA\",\"methods\":[\"get_return\",\"register_tokens\"]},{\"contract\":\"$LINEAR\",\"methods\":[\"deposit_and_stake\"]},{\"contract\":\"$USDT\"},{\"contract\":\"$MEME\"}],\"remove\":[]}"
  # The memecoin isn't on Rhea's global whitelist: the vault registers it for itself.
  x "$OWNER" act "{\"target\":\"$RHEA\",\"method\":\"register_tokens\",\"json_args\":\"{\\\"token_ids\\\":[\\\"$MEME\\\"]}\",\"one_yocto\":true}"
  send "$OWNER" "$VAULT" "$VAULT_FUND"
fi

if want oracle; then
  # Also for a vault that already exists: replaces its whole oracle config (owner only, through `x`).
  # Feeds for market floors and price triggers, all on the board, fresh = under 60 s, each within 2%
  # of its backup: the same asset from a different publisher, posted with it just before a step
  # (the keeper with --board; nothing is posted between steps). NVDA's backup is its USDG pool's
  # 30-min TWAP: it only confirms Chainlink's price (the floor is Chainlink's), so a bent TWAP
  # can stop a trade, never cheapen one. Closed market (weekends): Chainlink pauses, nothing trades.
  echo "== oracle: $VAULT reads usdc eth sol from $PBOARD, near nvda and every backup from $BOARD"
  need "$OWNER" 0.05
  feed() { # feed <name> <board> <price id> <backup price id (on $BOARD), or single_source>
    local b="{\"kind\":\"single_source\"}"
    [ "$4" = single_source ] || b="{\"kind\":\"other\",\"contract\":\"$BOARD\",\"price_id\":\"$4\"}"
    echo "{\"name\":\"$1\",\"contract\":\"$2\",\"price_id\":\"$3\",\"backup\":$b}"
  }
  x "$OWNER" oracle_setup "{\"config\":{\"max_age_s\":60,\"max_conf_bps\":100,\"tolerance_bps\":200,\"feeds\":[$(feed near "$BOARD" $NEAR_ID $NEAR_OP_ID),$(feed usdc "$PBOARD" $USDC_ID $USDC_ARB_ID),$(feed eth "$PBOARD" $ETH_ID $ETH_ARB_ID),$(feed sol "$PBOARD" $SOL_ID $SOL_ARB_ID),$(feed nvda "$BOARD" $NVDA_ID $NVDA_TWAP_ID)]}}"
fi

if want flow; then
  # No market floors here: testnet Rhea pools don't
  # track Pyth (1 wNEAR buys ~1.24 testnet USDT), so a floor would rightly refuse every swap.
  echo "== demo flow: 1 NEAR -> LiNEAR; 2 NEAR -> wrap -> half USDT, half memecoin; 0.5 NEAR DCA into LiNEAR every 10 min"
  need "$OWNER" 0.05
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
    # USDT has 24 decimals like wNEAR: at least 1.0 USDT per wNEAR (the pool pays ~1.24).
    {"input": wrap, "action": swap(1084, usdt, 1 * E24), "when": {"when": "now"}},
    # MEV has 18 decimals: the pool pays ~158,000 MEV per wNEAR; accept at least 120,000.
    {"input": wrap, "action": swap(2131, meme, 120_000 * 10**18), "when": {"when": "now"}},
    {"input": "near", "action": dict(stake, label="DCA into LiNEAR"), "when": {"when": "every", "every_ns": 600_000_000_000, "chunk": str(E24 // 10)}},
]
print(json.dumps({"name": "Testnet demo", "nodes": nodes}, separators=(",", ":")))
PY
)
  x "$OWNER" save_flow "$FLOW"
  x "$OWNER" fund "{\"flow_id\":0,\"node\":0,\"amount\":\"1$E24\"}"
  x "$OWNER" fund "{\"flow_id\":0,\"node\":1,\"amount\":\"2$E24\"}"
  x "$OWNER" fund "{\"flow_id\":0,\"node\":4,\"amount\":\"500000000000000000000000\"}"
fi

if want check; then
  # Read-only, also under DRY: balances and what each account's storage locks.
  echo "== check (views only)"
  echo "  account                                    free NEAR  storage NEAR  bytes"
  for a in "$BRAIN" "$FACTORY" "$BOARD" $([ "$PYTH_SOURCE" = core ] && echo "$PYTH_BOARD") "$OWNER" "$KEEPER" "$VAULT"; do printf '  %-42s %s\n' "$a" "$(rpc account "$a")"; done
  echo "  brain.get_factory:        $(rpc view "$BRAIN" get_factory)"
  echo "  factory.get_brain:        $(rpc view "$FACTORY" get_brain)"
  echo "  factory.get_code_hash:    $(rpc view "$FACTORY" get_code_hash)"
  echo "  factory.get_vault(owner): $(rpc view "$FACTORY" get_vault "{\"owner\":\"$OWNER\"}")"
  echo "  vault.brain_id:           $(rpc view "$VAULT" brain_id)"
  echo "  brain record (cost, paid): $(rpc view "$BRAIN" get_storage "{\"vault\":\"$VAULT\"}")"
  alloc_args='{"vault":"'"$VAULT"'","token":"near"}'
  echo "  brain allocated NEAR:     $(rpc view "$BRAIN" get_allocated "$alloc_args")"
  echo "  board.get_source:         $(rpc view "$BOARD" get_source)"
  [ "$PYTH_SOURCE" = core ] && echo "  pyth board.get_source:    $(rpc view "$PYTH_BOARD" get_source)"
  echo "  vault oracle (first 300): $(rpc view "$BRAIN" get_oracle "{\"vault\":\"$VAULT\"}")"
  for f in "near $NEAR_ID $BOARD" "near-op $NEAR_OP_ID $BOARD" "usdc $USDC_ID $PBOARD" "usdc-arb $USDC_ARB_ID $BOARD" "eth $ETH_ID $PBOARD" \
           "eth-arb $ETH_ARB_ID $BOARD" "sol $SOL_ID $PBOARD" "sol-arb $SOL_ARB_ID $BOARD" "nvda $NVDA_ID $BOARD" "nvda-twap $NVDA_TWAP_ID $BOARD"; do
    set -- $f
    echo "  $3 $1: $(rpc view "$3" get_price_unsafe "{\"price_id\":\"$2\"}")"
  done
  echo "  (missing or stale is normal between steps: prices are posted on demand. To post now:"
  echo "     cd scripts/keeper && KEEPER_ACCOUNT_ID=$KEEPER PYTH_API_KEY=... npm run pyth -- --board $PBOARD --vault $VAULT"
  echo "     npm run chainlink-price -- --board $BOARD --vault $VAULT and npm run twap-price -- --board $BOARD --vault $VAULT;"
  echo "     the live keeper: npm run flows -- --send --board $([ "$PYTH_SOURCE" = core ] && echo "$PYTH_BOARD,")$BOARD)"
fi

echo
echo "Brain:   https://testnet.nearblocks.io/address/$BRAIN"
echo "Factory: https://testnet.nearblocks.io/address/$FACTORY"
echo "Vault:   https://testnet.nearblocks.io/address/$VAULT"
