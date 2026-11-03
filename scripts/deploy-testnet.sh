#!/usr/bin/env bash
# Deploy the demo to NEAR TESTNET. Testnet only: mock tokens, no real value.
#
#   SUFFIX=wkhbz ./scripts/deploy-testnet.sh
#
# Expects five accounts created with the testnet faucet, keys saved by near-cli
# in ~/.near-credentials/testnet/ (never in this repo):
#   ll-owner-$SUFFIX  ll-market-$SUFFIX  ll-coin-$SUFFIX  ll-mwnear-$SUFFIX  ll-keeper-$SUFFIX
# Create one with:
#   near account create-account sponsor-by-faucet-service ll-owner-$SUFFIX.testnet \
#     autogenerate-new-keypair save-to-legacy-keychain network-config testnet create
#
# Then: build (make build), deploy 2 mock tokens + the market, register
# accounts, fund the market with 500M MCOIN + 4,000 mWNEAR, launch it, and give
# the keeper some inventory for the simulated market.
set -euo pipefail

: "${SUFFIX:?set SUFFIX, e.g. SUFFIX=wkhbz}"
OWNER="ll-owner-$SUFFIX.testnet"
MARKET="ll-market-$SUFFIX.testnet"
COIN="ll-coin-$SUFFIX.testnet"
WNEAR="ll-mwnear-$SUFFIX.testnet"
KEEPER="ll-keeper-$SUFFIX.testnet"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
E18="000000000000000000"
E24="000000000000000000000000"

call() { # call <signer> <contract> <method> <json> <deposit> [gas]
  near contract call-function as-transaction "$2" "$3" json-args "$4" \
    prepaid-gas "${6:-100.0 Tgas}" attached-deposit "$5" \
    sign-as "$1" network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: $1 -> $2.$3"
}
deploy() { # deploy <account> <wasm> <init json>
  near contract deploy "$1" use-file "$2" with-init-call new json-args "$3" \
    prepaid-gas '100.0 Tgas' attached-deposit '0 NEAR' \
    network-config testnet sign-with-legacy-keychain send >/dev/null
  echo "  ok: deployed $(basename "$2") to $1"
}

echo "1/5 build"
make -C "$ROOT" build >/dev/null

echo "2/5 mock tokens (MOCK: no value)"
deploy "$COIN" "$ROOT/contracts/target/near/mock_ft/mock_ft.wasm" \
  "{\"owner_id\":\"$OWNER\",\"total_supply\":\"1000000000$E18\",\"name\":\"Mock Coin (testnet)\",\"symbol\":\"MCOIN\",\"decimals\":18,\"call_fee_bps\":0}"
deploy "$WNEAR" "$ROOT/contracts/target/near/mock_ft/mock_ft.wasm" \
  "{\"owner_id\":\"$OWNER\",\"total_supply\":\"1000000$E24\",\"name\":\"Mock wNEAR (testnet)\",\"symbol\":\"mWNEAR\",\"decimals\":24}"

echo "3/5 market: native coin, 0.3% base fee, protocol slice only above 5,000 mWNEAR/day"
deploy "$MARKET" "$ROOT/contracts/target/near/market/market.wasm" \
  "{\"owner\":\"$OWNER\",\"token\":\"$COIN\",\"quote\":\"$WNEAR\",\"native\":true,\"fee_bps\":30,\"protocol_fee_after_daily_volume\":\"5000$E24\"}"

echo "4/5 register, fund, launch"
for token in "$COIN" "$WNEAR"; do
  for who in "$MARKET" "$KEEPER"; do
    call "$OWNER" "$token" storage_deposit "{\"account_id\":\"$who\"}" '0.01 NEAR'
  done
done
call "$OWNER" "$MARKET" storage_deposit '{}' '1 NEAR'
call "$KEEPER" "$MARKET" storage_deposit '{}' '1 NEAR'
call "$OWNER" "$COIN" ft_transfer_call "{\"receiver_id\":\"$MARKET\",\"amount\":\"500000000$E18\",\"msg\":\"{\\\"action\\\":\\\"fund\\\"}\"}" '1 yoctoNEAR' '150.0 Tgas'
call "$OWNER" "$WNEAR" ft_transfer_call "{\"receiver_id\":\"$MARKET\",\"amount\":\"4000$E24\",\"msg\":\"{\\\"action\\\":\\\"fund\\\"}\"}" '1 yoctoNEAR' '150.0 Tgas'
call "$OWNER" "$MARKET" launch_native '{"bins_per_side":230}' '0 NEAR' '300.0 Tgas'

echo "5/5 keeper inventory: 50M MCOIN + 2,000 mWNEAR"
call "$OWNER" "$COIN" ft_transfer "{\"receiver_id\":\"$KEEPER\",\"amount\":\"50000000$E18\"}" '1 yoctoNEAR'
call "$OWNER" "$WNEAR" ft_transfer "{\"receiver_id\":\"$KEEPER\",\"amount\":\"2000$E24\"}" '1 yoctoNEAR'

echo
echo "Done. Market: https://testnet.nearblocks.io/address/$MARKET"
near contract call-function as-read-only "$MARKET" get_config json-args '{}' network-config testnet now 2>/dev/null | sed -n '/{/,/}/p'
