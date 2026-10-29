#!/usr/bin/env bash
# The iOS app, end to end, by tapping: Alice and Bob on two simulators of
# their own (your demo simulators are left alone).
#   tests/ios-ui.sh
# Needs the regtest stack, demo/outside-world.py running and
# demo/ios/build-rust.sh run once.
set -euo pipefail
cd "$(dirname "$0")/../demo/ios"

curl -sf -m 5 127.0.0.1:8733/federation >/dev/null || { echo "start demo/outside-world.py first"; exit 1; }

sim() { # name -> udid, created on first use
  local id
  id=$(xcrun simctl list devices available | grep -F "$1 (" | grep -oE '[0-9A-F-]{36}' | head -1 || true)
  if [ -z "$id" ]; then id=$(xcrun simctl create "$1" "iPhone 16"); fi
  xcrun simctl boot "$id" 2>/dev/null || true
  echo "$id"
}
ALICE=$(sim "INDIGO UI Alice")
BOB=$(sim "INDIGO UI Bob")
# A full run starts both phones empty.
[ -n "${STEPS:-}" ] || for d in "$ALICE" "$BOB"; do xcrun simctl uninstall "$d" dev.indigo.demo 2>/dev/null || true; done

# Alice's words and what the two hand each other. Kept after the run so a
# failure can be inspected with the CLI (git-ignored, like every funded seed).
SHARE="$PWD/../../infra/.wallets/ios-ui"
mkdir -p "$SHARE" && chmod 700 "$SHARE"
DERIVED=build/uitest
xcodegen -q
xcodebuild build-for-testing -project IndigoDemo.xcodeproj -scheme IndigoDemo \
  -destination "id=$ALICE" -derivedDataPath "$DERIVED" -quiet

PASS=0 FAIL=0
step() { # device test
  local out
  if out=$(TEST_RUNNER_SHARE_DIR="$SHARE" xcodebuild test-without-building -project IndigoDemo.xcodeproj \
      -scheme IndigoDemo -destination "id=$1" -derivedDataPath "$DERIVED" \
      -only-testing:"IndigoDemoUITests/FlowTests/$2" 2>&1); then
    PASS=$((PASS + 1)); echo "ok   $2 ($(grep -o 'passed ([0-9.]* seconds)' <<<"$out" | tail -1))"
  else
    FAIL=$((FAIL + 1)); echo "FAIL $2"; grep -E "error:|XCTAssert|failed" <<<"$out" | grep -v "^Test Suite" | head -8 | sed 's/^/     /'
    echo "$PASS passed, $FAIL failed"; exit 1
  fi
}
# STEPS="testA1Alice" reruns part of it.
for s in ${STEPS:-testA1Alice testB1Bob testA2Alice testB2Bob}; do
  case $s in *Alice) step "$ALICE" "$s" ;; *) step "$BOB" "$s" ;; esac
done
echo "$PASS passed, $FAIL failed"
