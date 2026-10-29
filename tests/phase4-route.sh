#!/usr/bin/env bash
# Phase 4 (first part): automatic pocket choice and top-ups on regtest.
#   tests/phase4-route.sh regtest
# Needs the regtest stack and `cargo build -p indigo`.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$@"

total() { a balance | awk '{s += $2} END {print s}'; }
richest() { a balance | awk '$1 !~ /^ark-arkade/ {if ($2 > m) m = $2} END {print m}'; }
# Like `receive`, through `request` (the wallet picks the pocket).
request() { # amount
  a_exec request "$1" --wait "$WAIT" >"$WORK/recv.log" 2>&1 &
  local pid=$! inv
  inv=$(printed_invoice)
  [ -n "$inv" ] && pay_invoice "$inv"
  wait "$pid" || true
  cat "$WORK/recv.log"
}

echo "Phase 4 routing on $NETWORK"
a join "$FED_INVITE" --yes >/dev/null 2>&1
b join "$FED_INVITE" --yes >/dev/null 2>&1
receive 30000 >/dev/null
a move --from cashu --to fedimint 10000 --yes >/dev/null 2>&1

echo "4.r1 a small receive goes to ecash (Cashu)"
out=$(request 1000)
if grep -q "^into: *cashu" <<<"$out" && grep -q "^received 1000" <<<"$out"; then ok "1,000 sats received into Cashu"
else no "4.r1" "$out"; fi

echo "4.r2 pay an invoice without naming a pocket"
out=$(a send "$(invoice 3000)" 2>&1 || true)
if grep -q "^paid 3000 sats" <<<"$out" && ! grep -q "topped up" <<<"$out"; then ok "$(tail -1 <<<"$out")"
else no "4.r2" "$out"; fi

echo "4.r3 pay from a pocket the user picked"
out=$(a send "$(invoice 2000)" --from fedimint 2>&1 || true)
if grep -q "^paid 2000 sats.*from fedimint" <<<"$out"; then ok "paid from Fedimint as asked"
else no "4.r3" "$out"; fi

echo "4.r4 a picked pocket that is short is refused, nothing moves"
before=$(total)
out=$(a send "$(invoice 25000)" --from fedimint 2>&1 || true)
if grep -q "not enough funds" <<<"$out" && check "$(total) == $before"; then ok "refused: $(tail -1 <<<"$out")"
else no "4.r4" "$out / $before -> $(total)"; fi

echo "4.r5 an Ark address with an empty Ark pocket: topped up, then paid"
addr=$(b --rail ark-bark address 2>/dev/null | tail -1)
out=$(a send "$addr" 2000 2>&1 || true)
if grep -q "topped up" <<<"$out" && grep -q "^paid 2000 sats.*from ark-bark" <<<"$out"; then ok "$(tr '\n' ' ' <<<"$out")"
else no "4.r5" "$out"; fi
bob_ark=0
for _ in $(seq 20); do bob_ark=$(pocket b ark-bark 2>/dev/null || echo 0); check "${bob_ark:-0} >= 2000" && break; sleep 2; done
if check "${bob_ark:-0} >= 2000"; then ok "Bob has $bob_ark sats in Ark"; else no "4.r5 Bob" "Bob's Ark: $bob_ark"; fi

echo "4.r6 more than any one pocket holds: consolidated, then paid"
amount=$(python3 -c "print(int($(richest)) + 1000)")
if check "$(total) > $amount + 3000"; then
  out=$(a send "$(invoice "$amount")" 2>&1 || true)
  if grep -q "topped up" <<<"$out" && grep -q "^paid $amount sats" <<<"$out"; then ok "$(tr '\n' ' ' <<<"$out")"
  else no "4.r6" "$out"; fi
else no "4.r6" "not enough left to test: total $(total), richest $(richest)"; fi

echo "4.r7 ecash for Bob, pocket picked automatically"
token=$(a send-ecash 1500 2>/dev/null | tail -1)
out=$(b redeem "$token" 2>&1 || true)
if grep -q "^received 1500" <<<"$out"; then ok "Bob redeemed: $out"
else no "4.r7" "$out"; fi

echo "4.r8 more than the whole wallet: refused, nothing moves"
before=$(total)
out=$(a send "$(invoice 900000)" 2>&1 || true)
if grep -q "not enough funds" <<<"$out" && check "$(total) == $before"; then ok "refused"
else no "4.r8" "$out"; fi

echo "4.r9 the books match"
if audit_ok a && audit_ok b; then ok "both audits match"; else no "4.r9" "$(a audit; b audit)"; fi

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
