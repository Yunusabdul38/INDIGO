#!/usr/bin/env bash
# Phase 2 gate: N random moves between Cashu and Fedimint (default 50), many
# stopped at a random state or killed at a random moment, each finished by
# restarting the app. Passes when every move ends COMPLETED, FAILED or EXPIRED
# and the journal matches every pocket's balance to the msat.
#
#   tests/phase2-soak.sh regtest [moves]
set -euo pipefail

# shellcheck source=common.sh
source "$(dirname "$0")/common.sh" "$1"
MOVES=${2:-50}
STATES=(QUOTED DEST_INVOICE_CREATED AUTHORIZED SOURCE_PAYING SOURCE_PAID DEST_CLAIMED)

finish_all() { # restarts until no move is open, or gives up
  for _ in $(seq 120); do
    a balance >/dev/null 2>&1 || true
    # Captured first: with pipefail, `grep -q` exiting early fails the pipeline.
    intents=$(a intents)
    grep -qvE "^(COMPLETED|FAILED|EXPIRED) " <<<"$intents" || return 0
    sleep 2
  done
  return 1
}

echo "Phase 2 soak on $NETWORK: $MOVES moves"
a join "$FED_INVITE" --yes >/dev/null
receive 30000 >/dev/null
a move --from cashu --to fedimint 12000 --yes >/dev/null
a balance

for n in $(seq "$MOVES"); do
  if ((RANDOM % 2)); then from=cashu to=fedimint; else from=fedimint to=cashu; fi
  amount=$((200 + RANDOM % 800))
  case $((RANDOM % 3)) in
    0) how="no kill"; a move --from $from --to $to $amount --yes >/dev/null 2>&1 || true ;;
    1) at=${STATES[$((RANDOM % ${#STATES[@]}))]}; how="stopped at $at"
       INDIGO_TEST_STOP_AT=$at a move --from $from --to $to $amount --yes >/dev/null 2>&1 || true ;;
    2) delay=$(python3 -c "import random; print(round(random.uniform(0.5, 15), 1))"); how="killed after ${delay}s"
       a_exec move --from $from --to $to $amount --yes >/dev/null 2>&1 &
       pid=$!; sleep "$delay"; kill -9 "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true ;;
  esac
  if ! finish_all; then echo "move $n ($from -> $to $amount, $how): still open"; a intents | awk 'NR <= 3'; exit 1; fi
  printf '%3d %-8s -> %-8s %4d  %-32s %s\n' "$n" $from $to $amount "$how" "$(a intents | awk 'NR == 1 {print $1}')"
done

echo
a intents | awk '{print $1}' | sort | uniq -c
if a audit; then echo "PASS: $MOVES moves, every one final, journal matches to the msat"
else echo "FAIL: audit mismatch"; exit 1; fi
