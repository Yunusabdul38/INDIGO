#!/usr/bin/env bash
# Stops what demo/start.sh started:
#   demo/stop.sh          the outside world, the web wallet, both phones and
#                         the regtest stack; its chain, mint and federation
#                         data are kept for the next demo/start.sh
#   demo/stop.sh --wipe   the same, and deletes that data too: the next start
#                         is a brand-new chain (use demo/start.sh --fresh then,
#                         since the phones' wallets belong to the old one)
# Docker Desktop itself keeps running. Safe to run when things are already down.
set -uo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

step() { printf '\n\e[1;36m▸ %s\e[0m\n' "$*"; }
ok() { printf '  \e[32m✔\e[0m %s\n' "$*"; }

step "The outside world and the web wallet"
pkill -f "demo/outside-world.py" && ok "outside world stopped" || ok "outside world was not running"
pkill -f "http.server 8090" && ok "web wallet stopped" || ok "web wallet was not running"

step "Phones"
for phone in "iPhone 16" "iPhone 18 Pro"; do
  id=$(xcrun simctl list devices available | grep -F "    $phone (" | grep -oE '[0-9A-F-]{36}' | head -1)
  [ -n "$id" ] && xcrun simctl shutdown "$id" 2>/dev/null
  ok "$phone shut down"
done

step "Regtest stack"
if ! docker info >/dev/null 2>&1; then
  ok "Docker is not running: nothing to stop"
elif [ "${1:-}" = --wipe ]; then
  docker compose -f infra/docker-compose.regtest.yml down -v >/dev/null 2>&1
  ok "stopped, and its data deleted"
else
  docker compose -f infra/docker-compose.regtest.yml down >/dev/null 2>&1
  ok "stopped (data kept)"
fi

printf '\n\e[1;32mStopped.\e[0m Start again with demo/start.sh\n'
