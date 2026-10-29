#!/usr/bin/env bash
# Phase 3 on Second's public signet: the Bark pocket natively (the CLI) and in
# a browser (test 3.12: headless Chrome, its data in IndexedDB).
#
#   tests/phase3-signet.sh <native data dir> <native words file> <browser words file>
#
# The native wallet must hold on-chain signet coins (Second's faucet,
# https://signet.2nd.dev, needs a GitHub login). Needs `cargo build -p indigo`,
# the browser module built (see apps/web-ark-check/src/lib.rs), Node and Chrome.
# Signet blocks come every ~10 minutes, so boards take a while.
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
DIR=${1:?usage: $0 <native data dir> <native words file> <browser words file>}
WORDS=$(cat "${2:?native words file}")
BWORDS=$(cat "${3:?browser words file}")
PROFILE=$(mktemp -d)
PORT=8723
(cd "$ROOT/apps/web-ark-check" && exec python3 -m http.server "$PORT" --bind 127.0.0.1 >/dev/null 2>&1) &
SERVER=$!
# Background receives and Chrome must not outlive the run: they hold the
# wallets' storage.
trap 'kill $(jobs -p) 2>/dev/null; pkill -f "user-data-dir=$PROFILE" 2>/dev/null; rm -rf "$PROFILE"' EXIT

n() { INDIGO_TEST_MNEMONIC="$WORDS" "$ROOT/target/debug/indigo" --network signet --data-dir "$DIR" --rail ark-bark "$@"; }
# One browser action; prints the page's answer ("OK ..." or "FAIL ...").
web() { node "$ROOT/apps/web-ark-check/check.mjs" "http://localhost:$PORT" "$PROFILE" "$BWORDS" "$@" 2>/dev/null | tail -1 || true; }
ark() { n balance | awk '/^ark-bark:/ {print $2}'; }
wark() { web balance | sed -n 's/^OK \([0-9.]*\) sats spendable.*/\1/p'; }
check() { python3 -c "import sys; sys.exit(0 if ($1) else 1)"; }
# Waits until <command> prints a number >= <sats>, up to <minutes>.
wait_for() { # minutes sats command...
  local minutes=$1 sats=$2 got
  shift 2
  for _ in $(seq $((minutes * 2))); do
    got=$("$@" 2>/dev/null || echo 0)
    check "${got:-0} >= $sats" && return 0
    sleep 30
  done
  return 1
}
PASS=0
FAIL=0
ok() { echo "  PASS $1"; PASS=$((PASS + 1)); }
no() { echo "  FAIL $1: $2"; FAIL=$((FAIL + 1)); }

echo "Phase 3 on Second's signet (native + browser)"

echo "native: board"
if check "$(ark) < 100000"; then
  n ark board 150000 | tail -1
  if wait_for 60 140000 ark; then ok "native boarded: $(ark) sats in Ark"
  else no "native board" "Ark balance $(ark) after an hour"; fi
else ok "native already has $(ark) sats in Ark"; fi

echo "3.12 browser: open a wallet, IndexedDB"
address=$(web address)
if [[ "$address" == "OK "* ]]; then ok "the browser wallet opened (Ark address ${address:3:24}...)"
else no "3.12 open" "$address"; fi

echo "3.12/3.2 browser: Ark payment in and out"
n pay-address "${address#OK }" 20000 | tail -1
if wait_for 5 20000 wark; then
  out=$(web pay-address "$(n address | tail -1)" 1000)
  if grep -q "Completed" <<<"$out"; then ok "browser got 20,000 over Ark and paid 1,000 back"
  else no "3.2 browser pay" "$out"; fi
else no "3.2 browser receive" "browser balance $(wark)"; fi

echo "3.12/3.3 browser: Lightning receive"
# The browser page stays open, claiming, while the native wallet pays it.
node "$ROOT/apps/web-ark-check/check.mjs" "http://localhost:$PORT" "$PROFILE" "$BWORDS" receive-wait 5000 \
  >"$PROFILE/web-recv.log" 2>/dev/null &
for _ in $(seq 120); do invoice=$(sed -n 's/^INVOICE //p' "$PROFILE/web-recv.log"); [ -n "$invoice" ] && break; sleep 1; done
paid=$(n pay "$invoice" --yes 2>&1 | tail -1 || true)
wait || true
status=$(tail -1 "$PROFILE/web-recv.log")
if grep -q "Completed" <<<"$status"; then ok "browser received 5,000 over Lightning ($status)"
else no "3.3 browser" "$paid / $status"; fi

echo "3.12/3.4 browser: Lightning pay"
n receive 2000 --wait 120 >"$PROFILE/recv.log" 2>&1 &
for _ in $(seq 60); do invoice=$(sed -n 's/^invoice: //p' "$PROFILE/recv.log"); [ -n "$invoice" ] && break; sleep 1; done
out=$(web pay "$invoice")
wait || true
if grep -q "Completed" <<<"$out" && grep -q "^received 2000" "$PROFILE/recv.log"; then ok "browser paid 2,000 over Lightning"
else no "3.4 browser" "$out / $(tail -1 "$PROFILE/recv.log")"; fi

echo "3.12 browser: tab closed mid-payment, then reopened"
n receive 1000 --wait 180 >"$PROFILE/recv2.log" 2>&1 &
for _ in $(seq 60); do invoice=$(sed -n 's/^invoice: //p' "$PROFILE/recv2.log"); [ -n "$invoice" ] && break; sleep 1; done
KILL_AFTER_MS=4000 node "$ROOT/apps/web-ark-check/check.mjs" "http://localhost:$PORT" "$PROFILE" "$BWORDS" pay "$invoice" >/dev/null 2>&1 || true
# Reopening runs recovery: the payment must end paid or not paid, not stuck.
reopened=$(web recover)
wait || true
final=$(tail -1 "$PROFILE/recv2.log")
if [[ "$reopened" == "OK "* ]] && { grep -q "^received 1000" <<<"$final" || grep -q "not paid yet" <<<"$final"; }; then
  ok "reopened after the kill: $reopened; the payment ended: $final"
else no "3.12 closed tab" "$reopened / $final"; fi

echo "3.12/3.1 browser: board"
onchain=$(web onchain | sed -n 's/^OK \([^ ]*\).*/\1/p')
n ark send-onchain "$onchain" 30000 | tail -1
before=$(wark)
if wait_for 60 25000 sh -c "node '$ROOT/apps/web-ark-check/check.mjs' 'http://localhost:$PORT' '$PROFILE' '$BWORDS' onchain 2>/dev/null | sed -n 's/.*(\([0-9.]*\) sats).*/\1/p'"; then
  web board-all
  if wait_for 60 "$(python3 -c "print($before + 20000)")" wark; then ok "browser boarded: $before -> $(wark) sats"
  else no "3.1 browser board" "balance $(wark)"; fi
else no "3.1 browser" "the on-chain coins did not arrive"; fi

echo
echo "$PASS passed, $FAIL failed on signet"
[ "$FAIL" = 0 ]
