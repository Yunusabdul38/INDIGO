#!/usr/bin/env bash
# Lost phone: a wallet with money in every pocket is wiped, then brought back
# from its 12 words alone on a "new device" (an empty wallet folder).
#   tests/restore.sh regtest
# Needs the regtest stack and `cargo build -p indigo`.
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$@"

echo "Restore from the 12 words on $NETWORK"
a join "$FED_INVITE" --yes >/dev/null 2>&1
receive 20000 >/dev/null
a move --from cashu --to fedimint 6000 --yes >/dev/null 2>&1
a move --from cashu --to ark-bark 5000 --yes >/dev/null 2>&1
a send "$(invoice 1000)" --from cashu >/dev/null 2>&1
# Ecash Bob redeemed is gone for good. (A token nobody redeemed yet is still
# Alice's at the mint, so a restore brings it back.)
b redeem "$(a send-token 500 2>/dev/null | tail -1)" >/dev/null 2>&1
before=$(a balance 2>/dev/null)
echo "$before" | sed 's/^/  before: /'

rm -rf "$WORK/a" # the phone is lost
out=$(a restore --federation "$FED_INVITE" 2>&1 || true)
echo "$out" | sed 's/^/  restore: /'
after=$(a balance 2>/dev/null)

for p in cashu fedimint ark-bark; do
  was=$(awk -v p="$p:" 'index($1, p) == 1 {print $2}' <<<"$before")
  now=$(awk -v p="$p:" 'index($1, p) == 1 {print $2}' <<<"$after")
  if check "${now:-0} == ${was:-0} and ${was:-0} > 0"; then ok "$p: $now sats back"
  else no "$p" "had $was, restored ${now:-nothing}"; fi
done

if audit_ok a; then ok "the books match after restoring"; else no "audit" "$(a audit 2>&1)"; fi

out=$(a send "$(invoice 2000)" 2>&1 || true)
if grep -q "^paid 2000 sats" <<<"$out"; then ok "the restored wallet pays: $(tail -1 <<<"$out")"
else no "spend after restore" "$out"; fi

echo
echo "$PASS passed, $FAIL failed on $NETWORK"
[ "$FAIL" = 0 ]
