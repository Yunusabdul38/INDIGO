#!/usr/bin/env bash
# Phase 3 test guide (3.1-3.11) for the Ark pocket on Bark, on regtest.
#
#   tests/phase3.sh regtest     needs
#                               docker compose -f infra/docker-compose.regtest.yml up -d --wait
#                               && infra/regtest-setup.sh
#
# Needs `cargo build -p indigo`. 3.8-3.10 stop the Bark server (captaind) and
# start it again at the end. Each indigo run recovers first, so `a balance`
# is also how a test "restarts the app".
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$@"
[ "$NETWORK" = regtest ] || { echo "phase 3 runs on regtest (Bark's own server)" >&2; exit 2; }

R=--rail=ark-bark
btc() { "${COMPOSE[@]}" exec -T bitcoind bitcoin-cli -regtest -rpcuser=indigo -rpcpassword=indigo -rpcwallet=indigo "$@"; }
ark() { pocket "$1" ark-bark; }
# A fresh wallet from A's seed (3.7).
c() { INDIGO_TEST_MNEMONIC="$SEED_A" "$INDIGO" --network "$NETWORK" --data-dir "$WORK/c" "$@"; }
# Blocks, a few seconds apart, so the Bark server and wallets see each one.
blocks() { for _ in $(seq "$1"); do mine 1; sleep 1; done; }
# Restarts the app until `ark <wallet>` shows at least <sats>, or 2 minutes pass.
wait_ark() { # wallet sats
  for _ in $(seq 40); do
    check "$(ark "$1" 2>/dev/null || echo 0) >= $2" && return 0
    mine 1
    sleep 3
  done
  return 1
}
# The largest VTXO expiry height, from `ark vtxos`.
max_expiry() { a $R ark vtxos | sed -n 's/.*expires at block \([0-9]*\).*/\1/p' | sort -n | tail -1; }
captaind() { "${COMPOSE[@]}" "$1" captaind >/dev/null 2>&1; }
trap 'captaind start; rm -rf "$WORK"' EXIT

echo "Phase 3 (Bark) on $NETWORK"

echo "3.1 Board"
addr=$(a $R ark onchain | sed -n 's/^address: //p')
btc sendtoaddress "$addr" 0.003 >/dev/null
mine 1
start=$(date +%s)
out=$(a $R ark board 100000 2>&1 || true)
blocks 4
if grep -q "^boarding:" <<<"$out" && wait_ark a 99000 && audit_ok a; then
  ok "boarded 100,000 (fee $(calc "100000 - $(ark a)")) in $(($(date +%s) - start))s; journal matches"
else no "3.1" "$out / Ark balance $(ark a)"; fi

echo "3.2 Ark to Ark payment"
before=$(ark a)
start=$(date +%s)
paid=$(a $R pay-address "$(b $R address | tail -1)" 5000 2>&1 || true)
if grep -q "^paid 5000 sats" <<<"$paid" && [ "$(ark b)" = 5000 ] && audit_ok a && audit_ok b; then
  ok "B got 5,000 in $(($(date +%s) - start))s; A paid $(calc "$before - $(ark a)") (fee included)"
else no "3.2" "$paid / B has $(ark b)"; fi

echo "3.3 Lightning receive"
before=$(ark a)
out=$(receive 10000 $R)
got=$(calc "$(ark a) - $before")
if grep -q "^received 10000 sats" <<<"$out" && check "9800 <= $got <= 10000" && audit_ok a; then
  ok "received 10,000 over Lightning; balance +$got (server fee $(calc "10000 - $got"))"
else no "3.3" "$out / balance +$got"; fi

echo "3.4 Lightning pay"
before=$(ark a)
paid=$(a $R pay "$(invoice 3000)" --yes 2>&1 || true)
spent=$(calc "$before - $(ark a)")
if grep -q "^paid 3000 sats" <<<"$paid" && check "3000 <= $spent <= 3200" && audit_ok a; then
  ok "paid 3,000; balance -$spent (fee $(calc "$spent - 3000"))"
else no "3.4" "$paid / balance -$spent"; fi

echo "3.5 Moves with the other pockets"
a join "$FED_INVITE" --yes >/dev/null
recv=$(receive 10000 | tail -1)
in=$(a move --from cashu --to ark-bark 2000 --yes 2>&1 | tail -1 || true)
out=$(a move --from ark-bark --to fedimint 1000 --yes 2>&1 | tail -1 || true)
if grep -q "^COMPLETED" <<<"$in" && grep -q "^COMPLETED" <<<"$out" && audit_ok a; then
  ok "Cashu -> Ark and Ark -> Fedimint both COMPLETED; journal matches"
else no "3.5" "cashu receive: $recv / $in / $out / $(a intents | awk 'NR <= 2')"; fi

echo "3.6 Refresh"
old=$(max_expiry)
before=$(ark a)
a $R ark refresh --all >/dev/null 2>&1 || true
blocks 6
for _ in $(seq 20); do [ "$(max_expiry)" -gt "$old" ] 2>/dev/null && break; mine 1; sleep 3; done
new=$(max_expiry)
if [ "$new" -gt "$old" ] && audit_ok a; then
  ok "expiry block $old -> $new; balance $before -> $(ark a) (refresh fee only)"
else no "3.6" "expiry $old -> $new"; fi

echo "3.11 Kill tests (board, Lightning pay, refresh)"
kills_ok=1
btc sendtoaddress "$addr" 0.001 >/dev/null
mine 1
for case in "ark board 20000" "pay $(invoice 1000) --yes" "ark refresh --all"; do
  for delay in 0.5 2; do
    # shellcheck disable=SC2086 # the case is a command line
    a_exec $R $case >/dev/null 2>&1 &
    pid=$!
    sleep "$delay"
    kill -9 "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
    blocks 4
    a balance >/dev/null 2>&1 || true
    open=$(a ops | grep -c "^PENDING .*ark-bark" || true)
    if audit_ok a; then verdict="journal matches, $open open"; else verdict="AUDIT MISMATCH"; kills_ok=0; fi
    echo "    ${case%% *} killed at +${delay}s: $verdict"
  done
done
if [ "$kills_ok" = 1 ]; then ok "every kill recovered; balance and journal agree"
else no "3.11" "see above"; fi

echo "3.7 Q1a: restore from 12 words, server online"
want=$(ark a)
got=$(ark c 2>/dev/null || echo 0)
if [ "$got" = "$want" ]; then ok "a fresh wallet from A's 12 words has A's $want Ark sats back"
else no "3.7" "A has $want, restored from words: $got
A: $(a $R ark vtxos)
restored: $(c $R ark vtxos)"; fi

echo "3.8-3.10 need the server stopped; backups first"
# B's Bark folder before and after a payment, and on-chain coins for B's exit
# fees (an exit pays its transactions' fees from the on-chain wallet).
btc sendtoaddress "$(b $R ark onchain | sed -n 's/^address: //p')" 0.001 >/dev/null
blocks 2
b balance >/dev/null 2>&1 || true
cp -R "$WORK/b/bark" "$WORK/b-before"
b $R pay-address "$(a $R address | tail -1)" 1000 >/dev/null 2>&1 || true
b balance >/dev/null 2>&1 || true
cp -R "$WORK/b/bark" "$WORK/b-after"
size=$(du -sk "$WORK/b-after" | cut -f1)

# Restores B from its 12 words plus a copy of its Bark folder: restored <backup> <dir>
restored() { rm -rf "$WORK/$2" && mkdir -p "$WORK/$2" && cp -R "$WORK/$1" "$WORK/$2/bark"; }
d() { INDIGO_TEST_MNEMONIC="$SEED_B" "$INDIGO" --network "$NETWORK" --data-dir "$WORK/d" "$@"; }
e() { INDIGO_TEST_MNEMONIC="$SEED_B" "$INDIGO" --network "$NETWORK" --data-dir "$WORK/e" "$@"; }
# Exits everything and claims it to a fresh address; prints the BTC received.
exit_all() { # wallet
  local address status claimed=0
  address=$(btc getnewaddress)
  "$1" $R ark exit start >/dev/null 2>&1 || true
  for _ in $(seq 40); do
    status=$("$1" $R ark exit progress 2>&1 || true)
    grep -qE "exiting, [1-9][0-9.]* sats claimable" <<<"$status" && break
    blocks 10
  done
  "$1" $R ark exit claim "$address" >/dev/null 2>&1 || true
  # The claim goes out through the Bark server's bitcoind; it reaches ours by p2p.
  for _ in $(seq 15); do
    mine 1
    claimed=$(btc getreceivedbyaddress "$address" 1)
    check "$claimed > 0" && break
    sleep 2
  done
  echo "$claimed"
}

captaind stop

echo "3.8 Q1b: exit with the server offline"
got=$(exit_all a)
if check "$got > 0"; then ok "server off: A exited and claimed $got BTC on chain"
else no "3.8" "nothing claimed"; fi

echo "3.9 Q1c: restore from 12 words + backup, server offline"
restored b-after d
got=$(exit_all d)
if check "$got > 0"; then
  ok "B from its 12 words + its Bark folder (${size} KB, after its last payment) exited $got BTC without the server"
else no "3.9" "nothing claimed"; fi

echo "3.10 Backup freshness"
restored b-before e
got=$(exit_all e)
echo "  a backup from before B's last payment exited $got BTC"
echo "  (B's coins were already claimed in 3.9; what the old backup holds is"
echo "  the VTXO B spent in that payment)"
if check "$got == 0"; then ok "an old backup can't exit coins spent since: back up after every payment"
else ok "an old backup still exited $got BTC: recorded for the Phase 3 results"; fi

captaind start

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
