#!/usr/bin/env bash
# Everything the pitch needs, from a cold Mac, in one command:
#   demo/start.sh            Docker, the regtest stack, the outside world,
#                            the CLI, the app on two Simulators (Alice, Bob)
#                            and the web wallet on http://127.0.0.1:8090/wallet.html
#   demo/start.sh --fresh    the same, with both phones starting empty
# Safe to run again: whatever is already up is left as it is.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
RUN="$ROOT/demo/.run" # logs, git-ignored
mkdir -p "$RUN"
ALICE_PHONE="iPhone 16"
BOB_PHONE="iPhone 16 Pro"
APP=dev.indigo.demo

step() { printf '\n\e[1;36m▸ %s\e[0m\n' "$*"; }
ok() { printf '  \e[32m✔\e[0m %s\n' "$*"; }
fail() { printf '  \e[31m✘ %s\e[0m\n' "$*"; exit 1; }
waitfor() { # seconds what command...
  local end=$((SECONDS + $1)) what=$2; shift 2
  until "$@" >/dev/null 2>&1; do [ $SECONDS -lt $end ] || fail "$what did not come up"; sleep 2; done
}

step "Docker"
if ! docker info >/dev/null 2>&1; then open -a Docker; waitfor 180 Docker docker info; fi
ok "running"

step "Regtest stack (bitcoind, Lightning, Cashu mint, Fedimint, Ark server)"
# A Lightning node can take a minute to report healthy after a cold start.
for try in 1 2 3; do
  docker compose -f infra/docker-compose.regtest.yml up -d --wait >"$RUN/stack.log" 2>&1 && break
  [ $try = 3 ] && fail "the stack did not come up: see $RUN/stack.log"
done
infra/regtest-setup.sh >"$RUN/setup.log" 2>&1 || fail "setup failed: see $RUN/setup.log"
ok "up, channels open, federation and Ark server funded"

step "The outside world (faucet, shops, miners) on 127.0.0.1:8733"
if ! curl -sf -m 3 127.0.0.1:8733/federation >/dev/null; then
  nohup python3 demo/outside-world.py >"$RUN/outside-world.log" 2>&1 </dev/null &
  waitfor 30 "the outside world" curl -sf -m 3 127.0.0.1:8733/federation
fi
ok "listening"

step "Web wallet (the Ark pocket in the browser)"
if [ -f apps/web-ark-check/pkg/web_ark_check.js ]; then
  if ! curl -sf -m 3 127.0.0.1:8090/wallet.html >/dev/null; then
    nohup python3 -m http.server 8090 --bind 127.0.0.1 --directory apps/web-ark-check >"$RUN/web.log" 2>&1 </dev/null &
    waitfor 15 "the web wallet" curl -sf -m 3 127.0.0.1:8090/wallet.html
  fi
  ok "http://127.0.0.1:8090/wallet.html"
else
  printf '  – skipped: build the browser module first (see apps/web-ark-check/src/lib.rs)\n'
fi

step "CLI for the terminal demo"
cargo build -q -p indigo 2>"$RUN/cli.log" || fail "see $RUN/cli.log"
ok "target/debug/indigo"

step "iOS app on $ALICE_PHONE (Alice) and $BOB_PHONE (Bob)"
[ -d demo/ios/IndigoFFI.xcframework ] || demo/ios/build-rust.sh >"$RUN/rust.log" 2>&1 || fail "see $RUN/rust.log"
(cd demo/ios && xcodegen -q)
sim() { xcrun simctl list devices available | grep -F "    $1 (" | grep -oE '[0-9A-F-]{36}' | head -1; }
ALICE=$(sim "$ALICE_PHONE"); BOB=$(sim "$BOB_PHONE")
[ -n "$ALICE" ] && [ -n "$BOB" ] || fail "need Simulators named \"$ALICE_PHONE\" and \"$BOB_PHONE\""
for d in "$ALICE" "$BOB"; do xcrun simctl boot "$d" 2>/dev/null || true; done
xcodebuild build -project demo/ios/IndigoDemo.xcodeproj -scheme IndigoDemo -destination "id=$ALICE" \
  -derivedDataPath demo/ios/build/app -quiet >"$RUN/app.log" 2>&1 || fail "app build failed: see $RUN/app.log"
BUILT=demo/ios/build/app/Build/Products/Debug-iphonesimulator/IndigoDemo.app
for d in "$ALICE" "$BOB"; do
  [ "${1:-}" = --fresh ] && xcrun simctl uninstall "$d" "$APP" 2>/dev/null || true
  xcrun simctl install "$d" "$BUILT"
  xcrun simctl launch "$d" "$APP" >/dev/null
done
open -a Simulator 2>/dev/null || open /Applications/Xcode.app/Contents/Applications/DeviceHub.app
ok "running on both phones"

printf '\n\e[1;32mReady.\e[0m Pitch script: demo/PITCH.md · terminal demo: demo/indigo-demo.sh\n'
printf 'Web wallet: http://127.0.0.1:8090/wallet.html\n'
