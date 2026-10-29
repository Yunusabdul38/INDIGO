#!/usr/bin/env bash
# The web demo's wallet (the Ark pocket in headless Chrome, its data in
# IndexedDB) against the local regtest stack: paid over Lightning, pays an
# Ark address, pays a Lightning bill.
#   tests/web-regtest.sh
# Needs the regtest stack, `cargo build -p indigo`, the browser module built
# (see apps/web-ark-check/src/lib.rs), Node and Chrome.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" regtest
WEB="$ROOT/apps/web-ark-check"
PROFILE=$(mktemp -d)
PORT=8724
(cd "$WEB" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) &
trap 'kill $(jobs -p) 2>/dev/null; pkill -f "user-data-dir=$PROFILE" 2>/dev/null; rm -rf "$PROFILE" "$WORK"' EXIT
BWORDS=$("$INDIGO" new-test-seed 2>/dev/null | tail -1)

web() { NETWORK=regtest node "$WEB/check.mjs" "http://localhost:$PORT" "$PROFILE" "$BWORDS" "$@" 2>/dev/null | tail -1 || true; }
wbalance() { web balance | sed -n 's/^OK \([0-9.]*\) sats spendable.*/\1/p'; }

echo "Web wallet on regtest"

echo "w.1 paid over Lightning in the browser"
NETWORK=regtest node "$WEB/check.mjs" "http://localhost:$PORT" "$PROFILE" "$BWORDS" receive-wait 20000 >"$WORK/web.log" 2>/dev/null &
pid=$!
inv=""
for _ in $(seq 300); do inv=$(sed -n 's/^INVOICE //p' "$WORK/web.log"); [ -n "$inv" ] && break; sleep 0.2; done
[ -n "$inv" ] && pay_invoice "$inv"
wait "$pid" || true
got=$(wbalance)
# The Ark server keeps a fee on Lightning receives (about 140 sats here).
if grep -q "Completed" "$WORK/web.log" && check "${got:-0} >= 19500"; then ok "20,000 sats received, balance $got after the server's fee"
else no "w.1" "$(tail -2 "$WORK/web.log") / balance ${got:-?}"; fi

echo "w.2 the browser pays a CLI wallet's Ark address"
addr=$(a --rail ark-bark address 2>/dev/null | tail -1)
out=$(web pay-address "$addr" 5000)
for _ in $(seq 20); do have=$(pocket a ark-bark 2>/dev/null || echo 0); check "${have:-0} >= 5000" && break; sleep 2; done
if [[ $out == OK* ]] && check "${have:-0} >= 5000"; then ok "the CLI wallet has $have sats in Ark"
else no "w.2" "$out / CLI Ark: ${have:-?}"; fi

echo "w.3 the browser pays a Lightning bill"
out=$(web pay "$(invoice 2000)")
if [[ $out == OK*Completed* ]]; then ok "paid: $out"; else no "w.3" "$out"; fi

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
