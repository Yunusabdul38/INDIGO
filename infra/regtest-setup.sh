#!/usr/bin/env bash
# One-time setup after `docker compose -f infra/docker-compose.regtest.yml up -d`:
# funds a bitcoind wallet, runs DKG for the single-guardian federation, opens
# Lightning channels between the three nodes, connects the gateway, and funds
# the Bark (Ark) server and its Lightning node.
# Safe to re-run.
set -euo pipefail

COMPOSE=(docker compose -f "$(dirname "$0")/docker-compose.regtest.yml")
cli() { "${COMPOSE[@]}" exec -T bitcoind bitcoin-cli -regtest -rpcuser=indigo -rpcpassword=indigo "$@"; }
UI=http://127.0.0.1:18175
# fedimintd 0.12's own defaults. Older federations run the v1 set instead:
#   FEDIMINT_MODULES="mint wallet ln meta" infra/regtest-setup.sh
FEDIMINT_MODULES=${FEDIMINT_MODULES:-"mintv2 walletv2 lnv2 meta"}

# True once DKG has finished. The API port accepts WebSocket connections even
# before setup, so an answering port proves nothing; the invite code file only
# exists for a configured federation.
federation_ready() { "${COMPOSE[@]}" exec -T fedimintd test -s /data/invite-code; }
jar=$(mktemp)
trap 'rm -f "$jar"' EXIT

echo "bitcoind: wallet and initial blocks"
cli -rpcwait getblockchaininfo >/dev/null
cli createwallet indigo >/dev/null 2>&1 || cli loadwallet indigo >/dev/null 2>&1 || true
if [ "$(cli getblockcount)" -lt 101 ]; then
  cli generatetoaddress 101 "$(cli -rpcwallet=indigo getnewaddress)" >/dev/null
fi
echo "  height $(cli getblockcount)"

mine() { cli generatetoaddress "$1" "$(cli -rpcwallet=indigo getnewaddress)" >/dev/null; }

setup_federation() {
  # Any HTTP answer means the setup UI is listening (000 = not yet).
  for _ in $(seq 30); do
    [ "$(curl -s -o /dev/null -w '%{http_code}' "$UI/" || true)" != 000 ] && break
    sleep 1
  done
  curl -sf -c "$jar" -b "$jar" -o /dev/null -d password=indigo "$UI/login"
  # Modules must be listed explicitly: an empty list sets up a federation with none.
  module_args=()
  for m in $FEDIMINT_MODULES; do module_args+=(-d "enabled_modules=$m"); done
  curl -sf -c "$jar" -b "$jar" -o /dev/null \
    -d name=guardian-0 -d is_lead=true -d federation_name=indigo-regtest -d federation_size=1 \
    "${module_args[@]}" "$UI/"
  curl -sf -c "$jar" -b "$jar" -o /dev/null -X POST "$UI/start_dkg"
  for _ in $(seq 60); do
    federation_ready && return 0
    sleep 2
  done
  echo "  federation did not finish DKG; check: docker compose logs fedimintd" >&2
  exit 1
}

echo "fedimintd: setup"
federation_ready || setup_federation
INVITE=$("${COMPOSE[@]}" exec -T fedimintd cat /data/invite-code)
echo "  federation up: $INVITE"

echo "lightning: funds and channels"
ln() { local node=$1; shift; "${COMPOSE[@]}" exec -T "$node" lncli --network=regtest "$@"; }
field() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
pubkey() { ln "$1" getinfo | field '["identity_pubkey"]'; }
for node in lnd lnd-mint lnd-gateway; do
  if [ "$(ln "$node" walletbalance | field '["confirmed_balance"]')" -lt 5000000 ]; then
    cli -rpcwallet=indigo sendtoaddress "$(ln "$node" newaddress p2wkh | field '["address"]')" 1 >/dev/null
  fi
done
mine 6
for node in lnd lnd-mint lnd-gateway; do
  for _ in $(seq 60); do
    [ "$(ln "$node" walletbalance | field '["confirmed_balance"]')" -ge 5000000 ] && break
    sleep 1
  done
done
# from -> to, half of each channel pushed to the far side so both directions work.
open_channel() {
  local to_key
  to_key=$(pubkey "$2")
  if ln "$1" listchannels --peer "$to_key" | field '["channels"]' | grep -q chan_id; then return; fi
  ln "$1" connect "$to_key@$2:9735" >/dev/null 2>&1 || true
  ln "$1" openchannel --node_key "$to_key" --local_amt 2000000 --push_amt 1000000 >/dev/null
}
open_channel lnd lnd-mint
open_channel lnd lnd-gateway
open_channel lnd-mint lnd-gateway
# After a restart the containers have new IP addresses and LND keeps trying
# the old ones: reconnect every channel peer by name.
for pair in "lnd lnd-mint" "lnd lnd-gateway" "lnd-mint lnd-gateway"; do
  set -- $pair
  ln "$1" connect "$(pubkey "$2")@$2:9735" >/dev/null 2>&1 || true
done
mine 6
for node in lnd lnd-mint lnd-gateway; do
  for _ in $(seq 60); do
    [ "$(ln "$node" getinfo | field '["num_active_channels"]')" -ge 2 ] && break
    sleep 1
  done
  echo "  $node: $(ln "$node" getinfo | field '["num_active_channels"]') active channels"
done
# Gossip must reach every node before payments can be routed through a hop.
for _ in $(seq 60); do
  [ "$(ln lnd describegraph | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["edges"]))')" -ge 3 ] && break
  mine 1
  sleep 2
done

echo "gatewayd: connect to the federation"
gw() { "${COMPOSE[@]}" exec -T gatewayd gateway-cli --address http://127.0.0.1:8186 --rpcpassword indigo "$@"; }
# A new gateway waits for a seed before it starts; once set, this is refused.
gw cfg set-mnemonic >/dev/null 2>&1 || true
for _ in $(seq 30); do gw info >/dev/null 2>&1 && break; sleep 2; done
gw connect-fed "$INVITE" >/dev/null 2>&1 || true
# Clients only use gateways the guardians vetted (lnv2).
curl -sf -c "$jar" -b "$jar" -o /dev/null -d password=indigo "$UI/login"
curl -sf -c "$jar" -b "$jar" -o /dev/null -d gateway_url=http://127.0.0.1:18176 "$UI/lnv2/add" || true
# lnv2 gateways fund incoming payments from their own ecash, so the gateway
# deposits into the federation once.
FED_ID=$(gw info | field '["federations"][0]["federation_id"]')
gw_ecash() { gw get-balances | python3 -c 'import json,sys; b=json.load(sys.stdin)["ecash_balances"]; print(sum(x["ecash_balance_msats"] for x in b) // 1000)'; }
if [ "$(gw_ecash)" -lt 1000000 ]; then
  address=$(gw ecash pegin --federation-id "$FED_ID" | field '["address"]')
  cli -rpcwallet=indigo sendtoaddress "$address" 0.05 >/dev/null
  mine 11
  for _ in $(seq 60); do [ "$(gw_ecash)" -ge 1000000 ] && break; mine 1; sleep 2; done
fi
echo "  gateway ecash: $(gw_ecash) sats"
echo "  gateway: $(gw info | python3 -c 'import json,sys; d=json.load(sys.stdin); print(len(d.get("federations", [])), "federation(s)")')"

echo "bark: server funds and its Lightning channel"
cln() { "${COMPOSE[@]}" exec -T bark-cln lightning-cli --regtest "$@"; }
captain() { "${COMPOSE[@]}" exec -T captaind captaind --config /config/captaind.toml "$@"; }
for _ in $(seq 60); do cln getinfo >/dev/null 2>&1 && captain rpc wallet >/dev/null 2>&1 && break; sleep 3; done
# The server pays for rounds from its own on-chain wallet.
rounds=$(captain rpc wallet | field '["rounds"]["address"]')
if [ "$(captain rpc wallet | python3 -c 'import json,sys; print(json.load(sys.stdin)["rounds"].get("confirmed_balance", 0))' 2>/dev/null || echo 0)" = 0 ]; then
  cli -rpcwallet=indigo sendtoaddress "$rounds" 1 >/dev/null
fi
# Bark's Lightning goes through this node: a channel to lnd (the outside
# world), half pushed so it can both pay and be paid.
if ! cln listpeerchannels | grep -q "$(pubkey lnd)"; then
  cli -rpcwallet=indigo sendtoaddress "$(cln newaddr | field '["bech32"]')" 1 >/dev/null
  mine 6
  for _ in $(seq 60); do [ "$(cln listfunds | python3 -c 'import json,sys; print(len(json.load(sys.stdin)["outputs"]))')" -gt 0 ] && break; sleep 2; done
  cln connect "$(pubkey lnd)@lnd:9735" >/dev/null
  cln fundchannel "$(pubkey lnd)" 2000000 normal true 1 null 1000000000 >/dev/null
fi
mine 6
for _ in $(seq 60); do
  cln listpeerchannels | grep -q '"state": "CHANNELD_NORMAL"' && break
  mine 1
  sleep 2
done
echo "  bark server rounds wallet funded; cln channel: $(cln listpeerchannels | field '["channels"][0]["state"]' 2>/dev/null || echo none)"
