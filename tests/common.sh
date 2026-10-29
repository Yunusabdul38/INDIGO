# Shared by the test guides: network setup, wallets A and B, and helpers.
# Sourced with the network name as $1:  source tests/common.sh "$1"
#
# Each network gives the same helpers; only where invoices come from and who
# pays them differs:
#   regtest:   lnd is the outside world: it pays our invoices and issues the
#              ones we pay. The mint (lnd-mint) and the Fedimint gateway
#              (lnd-gateway) have nodes of their own.
#   mutinynet: the faucet's LNURL-withdraw pays our invoices, and its Lightning
#              address refund@lnurl.mutinynet.com issues the invoices we pay.

NETWORK=${1:?usage: $0 regtest|mutinynet}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
INDIGO="$ROOT/target/debug/indigo"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

case "$NETWORK" in
  regtest)
    COMPOSE=(docker compose -f "$ROOT/infra/docker-compose.regtest.yml")
    MINT_HOST=127.0.0.1:18085
    WAIT=30
    FED_INVITE=$("${COMPOSE[@]}" exec -T fedimintd cat /data/invite-code)
    # LND calls itself unsynced once the newest block is hours old, and the
    # Fedimint gateway stops serving until its node is synced: a fresh block
    # first, then wait for the gateway.
    mine() {
      "${COMPOSE[@]}" exec -T bitcoind bitcoin-cli -regtest -rpcuser=indigo -rpcpassword=indigo \
        -rpcwallet=indigo -generate "${1:-1}" >/dev/null
    }
    gateway_running() {
      "${COMPOSE[@]}" exec -T gatewayd gateway-cli --address http://127.0.0.1:8186 --rpcpassword indigo info 2>/dev/null \
        | grep -q '"gateway_state": "Running"'
    }
    wait_gateway() { for _ in $(seq 60); do gateway_running && return 0; sleep 2; done; return 1; }
    mine 1
    wait_gateway || { echo "the Fedimint gateway is not running: docker compose logs gatewayd" >&2; exit 2; }
    lnd() { "${COMPOSE[@]}" exec -T lnd lncli --network=regtest "$@"; }
    # lnd is the outside world; the mint has its own node (lnd-mint).
    # `sendpayment`: lncli's `payinvoice` fails on the gateway's invoices
    # ("invalid vertex length of 0") though they are valid.
    pay_invoice() { lnd sendpayment --pay_req "$1" --force >/dev/null 2>&1; }
    invoice() { lnd addinvoice --amt "$1" | python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_request"])'; }
    expired_invoice() { lnd addinvoice --amt "$1" --expiry 1 | python3 -c 'import json,sys; print(json.load(sys.stdin)["payment_request"])'; }
    ;;
  mutinynet)
    COMPOSE=(docker compose -f "$ROOT/infra/docker-compose.mutinynet.yml")
    MINT_HOST=127.0.0.1:28085
    WAIT=120
    # Written by infra/mutinynet-setup.sh once our federation is up.
    FED_INVITE=$(cat "$ROOT/infra/.mutinynet-federation-invite" 2>/dev/null || true)
    NODE_JAR="$WORK/node.jar"
    # shellcheck source=../infra/mutinynet-node.sh
    source "$ROOT/infra/mutinynet-node.sh"
    json() { python3 -c "import json,sys; print(json.load(sys.stdin)$1)"; }
    pay_invoice() { # the faucet's LNURL-withdraw pays it
      local req
      req=$(curl -sf https://faucet.mutinynet.com/api/lnurlw)
      curl -sf -G "$(json '["callback"]' <<<"$req")" --data-urlencode "k1=$(json '["k1"]' <<<"$req")" \
        --data-urlencode "pr=$1" | grep -q '"OK"'
    }
    invoice() { # from the faucet's Lightning address
      local callback
      callback=$(curl -sf https://lnurl.mutinynet.com/.well-known/lnurlp/refund | json '["callback"]')
      curl -sf -G "$callback" --data-urlencode "amount=$(($1 * 1000))" | json '["pr"]'
    }
    expired_invoice() { node_invoice "$1" 1; }
    ;;
  *) echo "unknown network $NETWORK: use regtest or mutinynet" >&2; exit 2 ;;
esac

PASS=0
FAIL=0
ok() { echo "  PASS $1"; PASS=$((PASS + 1)); }
no() { echo "  FAIL $1: $2"; FAIL=$((FAIL + 1)); }

# Wallet A and wallet B, each with its own seed and folder.
SEED_A=$("$INDIGO" new-test-seed)
SEED_B=$("$INDIGO" new-test-seed)
a() { INDIGO_TEST_MNEMONIC="$SEED_A" "$INDIGO" --network "$NETWORK" --data-dir "$WORK/a" "$@"; }
b() { INDIGO_TEST_MNEMONIC="$SEED_B" "$INDIGO" --network "$NETWORK" --data-dir "$WORK/b" "$@"; }
# For kill tests: `exec` makes the background job's PID the indigo process
# itself, so `kill -9 $!` kills the wallet, not a wrapping subshell.
a_exec() { INDIGO_TEST_MNEMONIC="$SEED_A" exec "$INDIGO" --network "$NETWORK" --data-dir "$WORK/a" "$@"; }
spendable() { "$@" balance | awk '{print $2}'; }
# One pocket's spendable balance: pocket <wallet> <cashu|fedimint>
pocket() { "$1" balance | awk -v p="$2:" 'index($1, p) == 1 {print $2}'; }
# Arithmetic on sat amounts with msat decimals: calc '<python expression>'
calc() { python3 -c "print(round($1, 3))"; }
# A comparison on sat amounts: check '<python expression>' (exit status)
check() { python3 -c "import sys; sys.exit(0 if ($1) else 1)"; }
audit_ok() { "$@" audit >/dev/null 2>&1; }

# Waits for `receive` to print its invoice into recv.log, then prints it.
printed_invoice() {
  local inv=""
  # Up to 2 minutes: a Fedimint invoice can take a while (gateway selection).
  for _ in $(seq 600); do
    inv=$(sed -n 's/^invoice: //p' "$WORK/recv.log")
    [ -n "$inv" ] && break
    sleep 0.2
  done
  echo "$inv"
}

# Starts `a receive`, pays its invoice once printed, waits for the result.
receive() { # amount [more indigo flags, e.g. --rail fedimint]
  a_exec receive "$1" --wait "$WAIT" "${@:2}" >"$WORK/recv.log" 2>&1 &
  local pid=$! inv
  inv=$(printed_invoice)
  [ -n "$inv" ] && pay_invoice "$inv"
  wait "$pid" || true
  cat "$WORK/recv.log"
}
