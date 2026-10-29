# Helpers for the management pages of our Mutinynet mint's Lightning node
# (cdk-mintd's built-in LDK node, see docker-compose.mutinynet.yml).
# Sourced by infra/mutinynet-setup.sh and tests/phase1.sh.

NODE_URL=http://127.0.0.1:28091
# Callers may point this into their own temp folder; otherwise remove it on exit.
NODE_JAR=${NODE_JAR:-$(mktemp)}

# The pages use a double-submit CSRF cookie: any GET sets it, every POST
# sends it back as the `_csrf` form field.
node_page() { curl -sfL -c "$NODE_JAR" -b "$NODE_JAR" "$NODE_URL$1"; }
node_post() {
  local path=$1 token
  shift
  node_page /onchain >/dev/null
  token=$(awk '$6 ~ /csrf/ {print $7}' "$NODE_JAR")
  curl -sf -c "$NODE_JAR" -b "$NODE_JAR" -X POST --data-urlencode "_csrf=$token" "$@" "$NODE_URL$path"
}

# A page's visible text on one line, for grepping.
node_text() {
  python3 -c 'import html,re,sys; t=re.sub(r"<script.*?</script>|<style.*?</style>","",sys.stdin.read(),flags=re.S); print(re.sub(r"\s+"," ",html.unescape(re.sub(r"<[^>]+>"," ",t))))'
}

# A Mutinynet invoice from our own node: amount_sats [expiry_seconds]
node_invoice() {
  node_post /invoices/bolt11 --data-urlencode "amount_btc=$1" ${2:+--data-urlencode "expiry_seconds=$2"} \
    | grep -oE 'lntbs[0-9a-z]+' | head -1
}
