#!/usr/bin/env bash
# Sets up our own Cashu mint on Mutinynet, with a Lightning channel, so the
# Mutinynet tests can run end to end. Safe to re-run: each step is skipped
# once done. The one manual step is funding the node from the faucet website.
#
#   infra/mutinynet-setup.sh
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(dirname "$HERE")
COMPOSE=(docker compose -f "$HERE/docker-compose.mutinynet.yml")
ESPLORA=https://mutinynet.com/api
# The faucet's Lightning node, as published on faucet.mutinynet.com.
FAUCET_NODE=02465ed5be53d04fde66c9418ff14a5f2267723810176c9212b722e542dc1afb1b
FAUCET_HOST=45.79.52.207
FAUCET_PORT=9735
CHANNEL_SATS=${CHANNEL_SATS:-500000}
# Pushed to the faucet's side at opening, so the mint can receive at once.
PUSH_SATS=${PUSH_SATS:-200000}

echo "1/4 seeds"
if [ ! -f "$HERE/.env.mutinynet" ]; then
  [ -x "$ROOT/target/debug/indigo" ] || cargo build -q -p indigo --manifest-path "$ROOT/Cargo.toml"
  printf 'CDK_MINTD_MNEMONIC="%s"\nCDK_MINTD_LDK_NODE_MNEMONIC="%s"\n' \
    "$("$ROOT/target/debug/indigo" new-test-seed)" "$("$ROOT/target/debug/indigo" new-test-seed)" >"$HERE/.env.mutinynet"
  chmod 600 "$HERE/.env.mutinynet"
  echo "  created infra/.env.mutinynet (git-ignored)"
else
  echo "  using infra/.env.mutinynet"
fi

echo "2/4 mint"
"${COMPOSE[@]}" up -d --wait >/dev/null
echo "  running on http://127.0.0.1:28085"

# shellcheck source=mutinynet-node.sh
source "$HERE/mutinynet-node.sh"
trap 'rm -f "$NODE_JAR"' EXIT
# Channels are listed on /balance ("N Active Channels", then one block per
# channel with its peer's node id).
lightning() { node_page /balance | node_text; }
channel_open() { grep -q "$FAUCET_NODE" <<<"$(lightning)"; }
channel_active() { grep -qE "(^| )[1-9][0-9]* Active Channels" <<<"$(lightning)"; }
wait_active() {
  for _ in $(seq 120); do
    channel_active && { echo "  ready"; exit 0; }
    sleep 15
  done
  echo "  channel not active after 30 minutes; check $NODE_URL/balance" >&2
  exit 1
}

if channel_open; then
  echo "3/4 funding: skipped, a channel to the faucet node exists"
  echo "4/4 channel: waiting until it is active"
  wait_active
fi

echo "3/4 funding"
# One funding address per setup, remembered so a re-run watches the same one.
if [ -s "$HERE/.mutinynet-funding-address" ]; then
  address=$(cat "$HERE/.mutinynet-funding-address")
else
  address=$(node_post /onchain/new-address | grep -oE 'tb1[02-9ac-hj-np-z]{20,90}' | head -1)
  echo "$address" >"$HERE/.mutinynet-funding-address"
fi
needed=$((CHANNEL_SATS + 20000))
echo "  send at least $needed sats to:"
echo
echo "      $address"
echo
echo "  from https://faucet.mutinynet.com (on-chain). Waiting for it to confirm..."
until [ "$(curl -sf "$ESPLORA/address/$address" | python3 -c 'import json,sys; s=json.load(sys.stdin)["chain_stats"]; print(s["funded_txo_sum"] - s["spent_txo_sum"])' 2>/dev/null || echo 0)" -ge "$needed" ]; do
  sleep 15
done
echo "  confirmed; waiting for the node to see it"
sleep 30

echo "4/4 channel to the faucet node"
node_post /channels/open --data-urlencode "node_id=$FAUCET_NODE" --data-urlencode "address=$FAUCET_HOST" \
  --data-urlencode "port=$FAUCET_PORT" --data-urlencode "amount_sats=$CHANNEL_SATS" \
  --data-urlencode "push_btc=$PUSH_SATS" >/dev/null
echo "  opened $CHANNEL_SATS sats ($PUSH_SATS pushed to the faucet side); waiting for confirmations"
wait_active
