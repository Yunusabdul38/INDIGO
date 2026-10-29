#!/usr/bin/env bash
# Sets up our own Fedimint federation and its Lightning gateway on Mutinynet,
# for the Phase 2 tests, and a channel between our mint and the gateway.
# Run after infra/mutinynet-setup.sh. Safe to re-run:
# each step is skipped once done. The one manual step is funding the
# gateway's node from the faucet website; the script stops and says when.
#
#   infra/mutinynet-federation-setup.sh
set -euo pipefail

HERE=$(cd "$(dirname "$0")" && pwd)
COMPOSE=(docker compose -f "$HERE/docker-compose.mutinynet.yml")
UI=http://127.0.0.1:28175
# The faucet's Lightning node, as in infra/mutinynet-setup.sh.
FAUCET_NODE=02465ed5be53d04fde66c9418ff14a5f2267723810176c9212b722e542dc1afb1b
FAUCET_HOST=45.79.52.207:9735
# The gateway's channel to the faucet node; half is pushed to the faucet so
# the gateway can receive at once. Plus ecash the gateway deposits into the
# federation: lnv2 gateways fund incoming payments from it.
CHANNEL_SATS=${CHANNEL_SATS:-400000}
PUSH_SATS=${PUSH_SATS:-200000}
ECASH_SATS=${ECASH_SATS:-200000}
# A direct channel from our mint's node to the gateway's, half pushed to the
# gateway: their channels to the faucet are unannounced and the gateway's
# invoices carry no route hints, so only a direct peer can pay the gateway.
MINT_CHANNEL_SATS=${MINT_CHANNEL_SATS:-60000}
NEEDED_SATS=$((CHANNEL_SATS + ECASH_SATS + 20000))

field() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
gw() { "${COMPOSE[@]}" exec -T gatewayd gateway-cli --address http://127.0.0.1:8186 --rpcpassword indigo "$@"; }
federation_ready() { "${COMPOSE[@]}" exec -T fedimintd test -s /data/invite-code; }

echo "1/6 federation"
"${COMPOSE[@]}" up -d --wait fedimintd gatewayd >/dev/null
if ! federation_ready; then
  jar=$(mktemp)
  trap 'rm -f "$jar"' EXIT
  curl -sf -c "$jar" -b "$jar" -o /dev/null -d password=indigo "$UI/login"
  curl -sf -c "$jar" -b "$jar" -o /dev/null -d name=guardian-0 -d is_lead=true \
    -d federation_name=indigo-mutinynet -d federation_size=1 \
    -d enabled_modules=mintv2 -d enabled_modules=walletv2 -d enabled_modules=lnv2 -d enabled_modules=meta "$UI/"
  curl -sf -c "$jar" -b "$jar" -o /dev/null -X POST "$UI/start_dkg"
  for _ in $(seq 60); do federation_ready && break; sleep 2; done
fi
INVITE=$("${COMPOSE[@]}" exec -T fedimintd cat /data/invite-code)
echo "$INVITE" >"$HERE/.mutinynet-federation-invite"
echo "  up: $INVITE (saved in infra/.mutinynet-federation-invite)"

echo "2/6 gateway"
# A new gateway waits for a seed before it starts; once set, this is refused.
gw cfg set-mnemonic >/dev/null 2>&1 || true
# Its LDK node syncs with Mutinynet first, which can take a few minutes.
for _ in $(seq 150); do gw info 2>/dev/null | grep -q '"gateway_state": "Running"' && break; sleep 2; done
gw connect-fed "$INVITE" >/dev/null 2>&1 || true
FED_ID=$(gw info | field '["federations"][0]["federation_id"]')
# Clients only use gateways the guardians vetted (lnv2).
jar=$(mktemp)
trap 'rm -f "$jar" "${NODE_JAR:-}"' EXIT
curl -sf -c "$jar" -b "$jar" -o /dev/null -d password=indigo "$UI/login"
curl -sf -c "$jar" -b "$jar" -o /dev/null -d gateway_url=http://127.0.0.1:28176 "$UI/lnv2/add" || true
echo "  connected to $FED_ID and vetted"

balances() { gw get-balances; }
channels() { gw lightning list-channels 2>/dev/null || echo '{"channels": []}'; }
has_channel() { channels | grep -q "$FAUCET_NODE"; }
ecash_sats() { balances | python3 -c 'import json,sys; print(sum(x["ecash_balance_msats"] for x in json.load(sys.stdin)["ecash_balances"]) // 1000)'; }
onchain_sats() { balances | field '["onchain_balance_sats"]'; }

echo "3/6 funding"
if has_channel && [ "$(ecash_sats)" -ge $((ECASH_SATS / 2)) ]; then
  echo "  skipped: channel and ecash in place"
elif [ "$(onchain_sats)" -lt "$NEEDED_SATS" ] && ! has_channel; then
  address=$(gw onchain address | tr -d '"{} \n' | sed 's/^address://')
  echo "$address" >"$HERE/.mutinynet-gateway-address"
  echo "  The gateway needs $NEEDED_SATS sats on chain. Send them from"
  echo "  https://faucet.mutinynet.com to:"
  echo "    $address"
  echo "  then run this script again (blocks come every 30 seconds)."
  exit 0
else
  echo "  $(onchain_sats) sats on chain"
fi

echo "4/6 gateway ecash in the federation"
# One on-chain spend at a time: started together, the deposit and the channel
# funding can pick the same coins, and one of them never confirms.
if [ "$(ecash_sats)" -lt $((ECASH_SATS / 2)) ]; then
  address=$(gw ecash pegin --federation-id "$FED_ID" | field '["address"]')
  gw onchain send --address "$address" --amount "$ECASH_SATS sats" --fee-rate-sats-per-vbyte 2 >/dev/null
  echo "  deposited $ECASH_SATS sats; waiting for the federation to credit it"
  for _ in $(seq 120); do [ "$(ecash_sats)" -ge $((ECASH_SATS / 2)) ] && break; sleep 15; done
fi
echo "  $(ecash_sats) sats of ecash"

echo "5/6 channel to the faucet node"
if ! has_channel; then
  gw lightning open-channel --pubkey "$FAUCET_NODE" --host "$FAUCET_HOST" \
    --channel-size-sats "$CHANNEL_SATS" --push-amount-sats "$PUSH_SATS" >/dev/null
  echo "  opening $CHANNEL_SATS sats, $PUSH_SATS pushed"
fi

echo "6/6 channel from our mint to the gateway"
# shellcheck source=mutinynet-node.sh
source "$HERE/mutinynet-node.sh"
GATEWAY_NODE=$(gw info | field '["lightning_info"]["connected"]["public_key"]')
if ! node_page /balance | node_text | grep -q "$GATEWAY_NODE"; then
  # The gateway's LDK node listens on 9736 in fedimintd's network.
  node_post /channels/open --data-urlencode "node_id=$GATEWAY_NODE" --data-urlencode "address=fedimintd" \
    --data-urlencode "port=9736" --data-urlencode "amount_sats=$MINT_CHANNEL_SATS" \
    --data-urlencode "push_btc=$((MINT_CHANNEL_SATS / 2))" >/dev/null
  echo "  opening $MINT_CHANNEL_SATS sats, half pushed to the gateway"
fi

active_channels() { channels | grep -c '"is_active": true' || true; }
for _ in $(seq 120); do
  if [ "$(ecash_sats)" -ge $((ECASH_SATS / 2)) ] && [ "$(active_channels)" -ge 2 ]; then
    echo "  ready: $(ecash_sats) sats of ecash; channels to the faucet and to our mint active"
    exit 0
  fi
  sleep 15
done
echo "  not ready after 30 minutes; check: docker compose -f infra/docker-compose.mutinynet.yml logs gatewayd" >&2
exit 1
