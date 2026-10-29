#!/usr/bin/env bash
# Phase 2 test guide (2.1-2.11): the Fedimint pocket, and moves between pockets.
#
#   tests/phase2.sh regtest     local stack; needs
#                               docker compose -f infra/docker-compose.regtest.yml up -d --wait
#                               && infra/regtest-setup.sh
#   tests/phase2.sh mutinynet   our own Mutinynet mint and federation; needs
#                               infra/mutinynet-setup.sh
#
# Both need `cargo build -p indigo`. Wallet A holds both pockets; wallet B is
# the other party for notes. Each indigo run recovers first, so `a balance`
# is also how a test "restarts the app".
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$@"
[ -n "$FED_INVITE" ] || { echo "no federation invite code for $NETWORK: run the setup script" >&2; exit 2; }

# Where a move stands: state <move id>
state() { a intents | grep -F "$1" | awk 'NR == 1 {print $1}'; }
# The id of the newest move.
last_move() { a intents | awk 'NR == 1' | grep -o 'mv-[0-9a-f]*'; }
# Restarts the app until move <id> is final or <seconds> pass; prints its state.
finish() { # id seconds
  local s=""
  for _ in $(seq $(($2 / 2))); do
    a balance >/dev/null 2>&1 || true
    s=$(state "$1")
    case "$s" in COMPLETED | FAILED | EXPIRED) break ;; esac
    sleep 2
  done
  echo "$s"
}
# The fee recorded on the newest operation of a kind: fee_of <ln_pay|ln_receive>
fee_of() { a ops | awk -v k="$1" '$2 == k {for (i = 1; i <= NF; i++) if ($i == "fee") {print $(i + 1); exit} print 0; exit}'; }

echo "Phase 2 on $NETWORK"

echo "2.1 Join federation"
out=$(a join "$FED_INVITE" --yes 2>&1)
b join "$FED_INVITE" --yes >/dev/null 2>&1
case "$NETWORK" in regtest) want=Regtest ;; *) want=Signet ;; esac
if grep -q "network: *$want" <<<"$out" && grep -q "^modules:" <<<"$out" && grep -q "^joined: fedimint:" <<<"$out"; then
  modules=$(sed -n 's/^modules: *//p' <<<"$out")
  ok "preview shown first (network $want, modules: $modules); Lightning module $(grep -q lnv2 <<<"$modules" && echo lnv2 || echo v1)"
else no "2.1" "$out"; fi

echo "2.2 Fedimint receive / pay"
receive 10000 --rail fedimint >/dev/null
got=$(pocket a fedimint); rfee=$(fee_of ln_receive)
inv=$(invoice 3000)
paid=$(a --rail fedimint pay "$inv" --yes 2>&1 || true)
after=$(pocket a fedimint); pfee=$(fee_of ln_pay)
if [ "$got" = "$(calc "10000 - $rfee")" ] && grep -q "^paid 3000 sats" <<<"$paid" \
  && [ "$after" = "$(calc "$got - 3000 - $pfee")" ] && audit_ok a; then
  ok "received 10,000 (gateway fee $rfee); paid 3,000 + fee $pfee; balance exact"
else no "2.2" "received $got (fee $rfee), then $after after paying (fee $pfee): $paid"; fi

echo "2.3 Notes round trip"
before=$(pocket a fedimint)
notes=$(a --rail fedimint send-token 2000)
out=$(b --rail fedimint receive-token "$notes" 2>&1 || true)
again=$(b --rail fedimint receive-token "$notes" 2>&1 || true)
if [[ "$notes" == fedimint* ]] && grep -q "^received 2000" <<<"$out" && grep -qi "already" <<<"$again" \
  && audit_ok a && audit_ok b; then
  ok "B gained $(pocket b fedimint) (notes round up to whole denominations); second redeem refused"
else no "2.3" "$out / $again"; fi

echo "2.4 Move Cashu -> Fedimint"
receive 20000 >/dev/null
c0=$(pocket a cashu); f0=$(pocket a fedimint)
out=$(a move --from cashu --to fedimint 5000 --yes 2>&1 || true)
c1=$(pocket a cashu); f1=$(pocket a fedimint)
pfee=$(fee_of ln_pay); rfee=$(fee_of ln_receive)
if grep -q "^fee reserve:" <<<"$out" && grep -q "^COMPLETED" <<<"$(tail -1 <<<"$out")" \
  && [ "$c1" = "$(calc "$c0 - 5000 - $pfee")" ] && [ "$f1" = "$(calc "$f0 + 5000 - $rfee")" ] && audit_ok a; then
  ok "fee shown first; Cashu -5,000 - $pfee, Fedimint +5,000 - $rfee (gateway); COMPLETED"
else no "2.4" "cashu $c0 -> $c1, fedimint $f0 -> $f1: $out"; fi

echo "2.5 Move Fedimint -> Cashu"
c0=$(pocket a cashu); f0=$(pocket a fedimint)
out=$(a move --from fedimint --to cashu 2000 --yes 2>&1 || true)
c1=$(pocket a cashu); f1=$(pocket a fedimint)
pfee=$(fee_of ln_pay)
if grep -q "^COMPLETED" <<<"$(tail -1 <<<"$out")" && [ "$c1" = "$(calc "$c0 + 2000")" ] \
  && [ "$f1" = "$(calc "$f0 - 2000 - $pfee")" ] && audit_ok a; then
  ok "Fedimint -2,000 - $pfee, Cashu +2,000; COMPLETED"
else no "2.5" "cashu $c0 -> $c1, fedimint $f0 -> $f1: $out"; fi

echo "2.6 Kill at every state (Cashu -> Fedimint, 1,000 each)"
kills_ok=1
for at in QUOTED DEST_INVOICE_CREATED AUTHORIZED SOURCE_PAYING SOURCE_PAID DEST_CLAIMED; do
  c0=$(pocket a cashu)
  INDIGO_TEST_STOP_AT=$at a move --from cashu --to fedimint 1000 --yes >/dev/null 2>&1 || true
  id=$(last_move)
  end=$(finish "$id" 180)
  c1=$(pocket a cashu)
  case "$end" in
    COMPLETED) check "$c0 - $c1 >= 1000" && verdict=ok || verdict=bad ;;
    FAILED | EXPIRED) [ "$c1" = "$c0" ] && verdict=ok || verdict=bad ;;
    *) verdict=bad ;;
  esac
  audit_ok a || verdict=bad
  echo "    stopped at $at: $end, Cashu $c0 -> $c1, journal $([ "$verdict" = ok ] && echo matches || echo CHECK)"
  [ "$verdict" = ok ] || kills_ok=0
done
if [ "$kills_ok" = 1 ]; then ok "every stop ended COMPLETED, or FAILED/EXPIRED with the money still in Cashu"
else no "2.6" "see above"; fi

echo "2.7 Destination down"
if [ "$NETWORK" = regtest ]; then
  f0=$(pocket a fedimint)
  "${COMPOSE[@]}" stop cdk-mint >/dev/null 2>&1
  out=$(a move --from fedimint --to cashu 1000 --yes 2>&1 || true)
  "${COMPOSE[@]}" start cdk-mint >/dev/null 2>&1
  for _ in $(seq 30); do curl -sf "http://$MINT_HOST/v1/info" >/dev/null && break; sleep 1; done
  if [ "$(state "$(last_move)")" = FAILED ] && [ "$(pocket a fedimint)" = "$f0" ]; then
    ok "FAILED before an invoice existed; Fedimint balance untouched"
  else no "2.7" "$out"; fi
else echo "  SKIP regtest only: it stops a service"; fi

echo "2.8 Source fails mid-pay"
if [ "$NETWORK" = regtest ]; then
  f0=$(pocket a fedimint)
  INDIGO_TEST_PAUSE_AT=AUTHORIZED:8 a move --from fedimint --to cashu 1000 --yes --wait 5 >"$WORK/move.log" 2>&1 &
  pid=$!
  for _ in $(seq 100); do grep -q "paused at AUTHORIZED" "$WORK/move.log" 2>/dev/null && break; sleep 0.2; done
  "${COMPOSE[@]}" stop gatewayd >/dev/null 2>&1
  wait "$pid" || true
  "${COMPOSE[@]}" start gatewayd >/dev/null 2>&1
  wait_gateway || true
  end=$(finish "$(last_move)" 180)
  if [ "$end" = FAILED ] && [ "$(pocket a fedimint)" = "$f0" ] && audit_ok a; then
    ok "gateway gone after AUTHORIZED: FAILED, Fedimint balance intact"
  else no "2.8" "$end, fedimint $f0 -> $(pocket a fedimint): $(cat "$WORK/move.log")"; fi
else echo "  SKIP regtest only: it stops a service"; fi

echo "2.9 Paid but not yet claimed"
f0=$(pocket a fedimint)
INDIGO_TEST_STOP_AT=SOURCE_PAID a move --from cashu --to fedimint 1000 --yes >/dev/null 2>&1 || true
id=$(last_move)
mid=$(state "$id")
end=$(finish "$id" 60)
if [ "$mid" = SOURCE_PAID ] && [ "$end" = COMPLETED ] && check "$(pocket a fedimint) > $f0"; then
  ok "stopped at SOURCE_PAID; the restart claimed it: COMPLETED"
else no "2.9" "$mid -> $end"; fi

echo "2.10 Idempotency"
c0=$(pocket a cashu)
a move --from cashu --to fedimint 1000 --key phase2-once --yes >/dev/null 2>&1 || true
again=$(a move --from cashu --to fedimint 1000 --key phase2-once --yes 2>&1 || true)
c1=$(pocket a cashu)
moved=$(calc "$c0 - $c1")
if grep -q "^COMPLETED" <<<"$again" && check "1000 <= $moved < 1100"; then
  ok "same key twice: one move ($moved sats left Cashu, fee included)"
else no "2.10" "$moved left Cashu: $again"; fi

echo "2.11 Accounting"
if a audit && b audit; then ok "journal matches balances to the msat, in every pocket"
else no "2.11" "audit mismatch"; fi

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
