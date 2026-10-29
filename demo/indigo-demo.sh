#!/usr/bin/env bash
# INDIGO pitch demo: one wallet, every Bitcoin pocket, money moving end to end
# on a local regtest network (real Lightning, real ecash, real Ark).
#
#   demo/indigo-demo.sh            press Enter at each step
#   DEMO_AUTO=1 demo/indigo-demo.sh   run straight through
#
# Needs the regtest stack (docker compose -f infra/docker-compose.regtest.yml
# up -d --wait && infra/regtest-setup.sh) and `cargo build -p indigo`.
set -euo pipefail

# shellcheck source=../tests/common.sh
source "$(dirname "$0")/../tests/common.sh" regtest

B_=$'\e[1m'; DIM=$'\e[2m'; G=$'\e[32m'; C=$'\e[36m'; Y=$'\e[33m'; R=$'\e[0m'
step=0
title() { step=$((step + 1)); printf '\n%s━━ %d. %s ━━%s\n' "$B_$C" "$step" "$1" "$R"; }
say() { printf '%s\n' "$*"; }
done_() { printf '%s✔ %s%s\n' "$G" "$*" "$R"; }
pause() { [ "${DEMO_AUTO:-}" = 1 ] && return; printf '%s(press Enter)%s' "$DIM" "$R"; read -r _; }

# Pocket balances for a wallet, with friendly names.
pockets() { # wallet label
  printf '%s%s%s\n' "$Y" "$2" "$R"
  "$1" balance 2>/dev/null | awk '
    /^cashu:/    { printf "   Cashu (ecash mint)        %12s sats\n", $2 }
    /^fedimint:/ { printf "   Fedimint (federation)     %12s sats\n", $2 }
    /^ark-bark:/ { printf "   Ark (Bark, self-custody)  %12s sats\n", $2 }'
}
show() { pockets a "Alice's wallet"; [ "${1:-}" = both ] && pockets b "Bob's wallet"; true; }
btc() { "${COMPOSE[@]}" exec -T bitcoind bitcoin-cli -regtest -rpcuser=indigo -rpcpassword=indigo -rpcwallet=indigo "$@"; }
moved() { tail -1 | awk '{print $1}'; }

clear 2>/dev/null || true
printf '%sINDIGO%s — one SDK for every Bitcoin layer\n' "$B_" "$R"
say "Alice's app uses INDIGO. Her wallet holds three pockets: Cashu ecash,"
say "a Fedimint federation, and Ark. Lightning connects them all."
say "${DIM}(local regtest network: real Lightning nodes, a real mint, federation and Ark server)${R}"
a join "$FED_INVITE" --yes >/dev/null 2>&1
b join "$FED_INVITE" --yes >/dev/null 2>&1
show
pause

title "Alice gets paid 50,000 sats over Lightning, into Cashu"
say "Someone on the Lightning network pays Alice's invoice."
out=$(receive 50000)
grep -q "^received 50000" <<<"$out" && done_ "received 50,000 sats"
show
pause

title "Alice moves 20,000 sats from Cashu to Fedimint"
say "One call. INDIGO has Fedimint issue an invoice and Cashu pay it,"
say "saving every step, so a crash can never lose or double-pay money."
[ "$(a move --from cashu --to fedimint 20000 --yes 2>&1 | moved)" = COMPLETED ] && done_ "moved"
show
pause

title "Alice moves 10,000 sats from Fedimint into Ark"
say "Ark keeps her money self-custodial: she can always exit to Bitcoin herself."
[ "$(a move --from fedimint --to ark-bark 10000 --yes 2>&1 | moved)" = COMPLETED ] && done_ "moved"
show
pause

title "Alice brings on-chain bitcoin into Ark (boarding)"
addr=$(a --rail ark-bark ark onchain 2>/dev/null | sed -n 's/^address: //p')
btc sendtoaddress "$addr" 0.0005 >/dev/null
mine 1
say "0.0005 BTC arrives on chain; Alice boards 30,000 sats of it into Ark."
a --rail ark-bark ark board 30000 >/dev/null 2>&1
for _ in $(seq 30); do
  mine 1
  sleep 2
  check "$(pocket a ark-bark 2>/dev/null || echo 0) > 35000" && break
done
done_ "boarded"
show
pause

title "Alice pays a Lightning bill of 3,000 sats from her Ark pocket"
paid=$(a --rail ark-bark pay "$(invoice 3000)" --yes 2>&1 | grep "^paid" || true)
done_ "$paid"
show
pause

title "Alice hands Bob ecash: a Cashu token and Fedimint notes"
say "Bearer money, sent as text (a QR code, a chat message), even offline."
token=$(a send-token 2000 2>/dev/null)
say "${DIM}Cashu token: ${token:0:48}...${R}"
b receive-token "$token" >/dev/null 2>&1 && done_ "Bob redeemed 2,000 sats of Cashu"
notes=$(a --rail fedimint send-token 1000 2>/dev/null)
say "${DIM}Fedimint notes: ${notes:0:48}...${R}"
b --rail fedimint receive-token "$notes" >/dev/null 2>&1 && done_ "Bob redeemed 1,000 sats of Fedimint notes"
show both
pause

title "Alice pays Bob directly over Ark"
paid=$(a --rail ark-bark pay-address "$(b --rail ark-bark address 2>/dev/null | tail -1)" 2000 2>&1 | grep "^paid" || true)
done_ "$paid"
show both
pause

title "Every sat accounted for"
say "INDIGO keeps a journal of every movement. It matches each pocket's balance exactly:"
a audit 2>/dev/null | awk '{printf "   %-10s balance %10s   journal %10s   %s\n", substr($1, 1, index($1, ":") - 1), $3, $5, $NF}'
say
say "Moves Alice made, each one COMPLETED or safely rolled back:"
a intents 2>/dev/null | awk '{printf "   %-10s %8s sats  %s -> %s\n", $1, $2, substr($3, 1, index($3, ":") - 1), substr($5, 1, index($5, ":") - 1)}'
printf '\n%sOne SDK. Every Bitcoin layer. Safe money movement and exact accounting.%s\n' "$B_" "$R"
