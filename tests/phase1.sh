#!/usr/bin/env bash
# Phase 1 test guide (1.1-1.10): the Cashu pocket end to end.
#
#   tests/phase1.sh regtest     local stack; needs
#                               docker compose -f infra/docker-compose.regtest.yml up -d --wait
#                               && infra/regtest-setup.sh
#   tests/phase1.sh mutinynet   our own Mutinynet mint with real Lightning; needs
#                               infra/mutinynet-setup.sh
#
# Both need `cargo build -p indigo`. The same ten tests run on each network;
# tests/common.sh says where invoices come from and who pays them.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$@"

# Waits until the wallet has no payment left in flight (each CLI run recovers).
settle() {
  local ops
  for _ in $(seq 60); do
    # Capture first: with pipefail, `grep -q` closing the pipe early makes the
    # pipeline "fail" exactly when a match was found.
    # `ops` only reads the log; `balance` runs recovery first.
    a balance >/dev/null 2>&1 || true
    ops=$(a ops 2>/dev/null)
    grep -q "^PENDING .*ln_pay" <<<"$ops" || return 0
    sleep 2
  done
  return 1
}

echo "Phase 1 on $NETWORK"

echo "1.1 Receive over Lightning"
out=$(receive 10000)
if grep -q "received 10000 sats" <<<"$out" && [ "$(spendable a)" = 10000 ] && grep -q "^COMPLETED .*ln_receive" <<<"$(a ops)"; then
  ok "balance 10,000, operation COMPLETED"
else no "1.1" "$out"; fi

echo "1.2 Pay a Lightning bill"
before=$(spendable a)
out=$(a pay "$(invoice 3000)" --yes)
fee=$(sed -n 's/^paid 3000 sats, fee \([0-9]*\) sats$/\1/p' <<<"$out")
if [ -n "$fee" ] && [ "$(spendable a)" = $((before - 3000 - fee)) ] && grep -q "fee reserve" <<<"$out"; then
  ok "quote shown first; balance dropped by 3,000 + fee $fee exactly"
else no "1.2" "$out"; fi

echo "1.3 Fee cap"
before=$(spendable a)
if out=$(a pay "$(invoice 2000)" --max-fee 0 --yes 2>&1); then no "1.3" "paid despite cap: $out"
elif grep -q "above the cap" <<<"$out" && [ "$(spendable a)" = "$before" ]; then ok "refused before paying; balance unchanged"
else no "1.3" "$out"; fi

echo "1.4 Send ecash token"
before=$(spendable a)
token=$(a send-token 2000)
if [[ "$token" == cashu* ]] && [ "$(spendable a)" = $((before - 2000)) ]; then ok "token printed; balance down 2,000"
else no "1.4" "$token"; fi

echo "1.5 Receive ecash token"
out=$(b receive-token "$token")
again=$(b receive-token "$token" 2>&1 || true)
if grep -q "received 2000 sats" <<<"$out" && grep -qi "already spent" <<<"$again" && [ "$(spendable b)" = 2000 ]; then
  ok "B gained 2,000; second redeem refused: already spent"
else no "1.5" "$out / $again"; fi

echo "1.6 Kill during receive"
before=$(spendable a)
a_exec receive 4000 --wait "$WAIT" >"$WORK/recv.log" 2>&1 &
pid=$!
inv=$(printed_invoice)
kill -9 "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
# Paid while the wallet is down; the next start must claim it.
[ -n "$inv" ] && pay_invoice "$inv"
after=$before
for _ in $(seq 30); do
  after=$(spendable a 2>/dev/null)
  [ "$after" = $((before + 4000)) ] && break
  sleep 2
done
if [ "$after" = $((before + 4000)) ] && audit_ok a; then ok "killed after the invoice printed; restart claimed 4,000"
else no "1.6" "balance $before -> $after; $(cat "$WORK/recv.log")"; fi

echo "1.7 Kill during pay (six kill points across the payment)"
# A payment whose outcome is ambiguous stays PENDING until its quote expires,
# then must end paid or failed.
kills_ok=1
for delay in 0 0.05 0.2 0.5 1 2; do
  before=$(spendable a 2>/dev/null)
  inv=$(invoice 300)
  a_exec pay "$inv" --yes >"$WORK/pay.log" 2>&1 &
  pid=$!
  for _ in $(seq 100); do grep -q "most it can cost" "$WORK/pay.log" 2>/dev/null && break; sleep 0.02; done
  sleep "$delay"
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  if ! settle; then kills_ok=0; no "1.7" "payment still pending after its quote expired (kill +${delay}s)"; break; fi
  after=$(spendable a 2>/dev/null)
  state=$(awk '$2 == "ln_pay" && $3 == 300 {print $1; exit}' <<<"$(a ops)")
  spent=$((before - after))
  # Paid: 300 plus a small fee left the wallet. Failed: nothing did.
  if ! audit_ok a || { [ "$state" = COMPLETED ] && { [ "$spent" -lt 300 ] || [ "$spent" -gt 320 ]; }; } \
    || { [ "$state" != COMPLETED ] && [ "$spent" != 0 ]; }; then
    kills_ok=0
    no "1.7" "kill +${delay}s: state ${state:-unrecorded}, balance $before -> $after
$(cat "$WORK/pay.log")"
    break
  fi
  if grep -q "^paid" "$WORK/pay.log"; then when="after the payment finished"; else when="mid-payment"; fi
  echo "    kill +${delay}s ($when): ${state:-not recorded}, $spent sats left the wallet, journal matches"
done
[ "$kills_ok" = 1 ] && ok "every kill ended paid or failed; never stuck, never double-paid"

echo "1.8 Mint offline"
"${COMPOSE[@]}" stop cdk-mint >/dev/null 2>&1
bal=$(a balance 2>&1 || true)
pay=$(a pay "$(invoice 500)" --yes 2>&1 || true)
# Not `up --wait`: on Mutinynet the mint's node can fail its first start
# (Esplora timing out) and is restarted by its policy; wait until it answers.
"${COMPOSE[@]}" start cdk-mint >/dev/null 2>&1 || true
for _ in $(seq 90); do curl -sf "http://$MINT_HOST/v1/info" >/dev/null && break; sleep 2; done
if grep -q "stale" <<<"$bal" && grep -q "$MINT_HOST" <<<"$pay"; then ok "balance marked stale; pay error names the mint"
else no "1.8" "balance: $bal / pay: $pay"; fi

echo "1.9 Expired bill"
expired=$(expired_invoice 700)
sleep 2
before=$(spendable a)
if out=$(a pay "$expired" --yes 2>&1); then no "1.9" "paid an expired invoice"
elif grep -qi "expired" <<<"$out" && [ "$(spendable a)" = "$before" ]; then ok "refused before any money moved"
else no "1.9" "$out"; fi

echo "1.10 Accounting"
if a audit && b audit; then ok "journal matches balances to the sat"; else no "1.10" "audit mismatch"; fi

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
