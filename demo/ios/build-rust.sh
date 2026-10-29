#!/usr/bin/env bash
# Builds INDIGO for the iOS Simulator and generates its Swift bindings:
# IndigoFFI.xcframework and Generated/indigo_ffi.swift, then the Xcode project.
set -euo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$HERE/../.." && pwd)
cd "$ROOT"
cargo build -p indigo-ffi --lib --release --target aarch64-apple-ios-sim
LIB="$ROOT/target/aarch64-apple-ios-sim/release/libindigo_ffi.a"
rm -rf "$HERE/Generated" "$HERE/IndigoFFI.xcframework"
cargo run -q -p indigo-ffi --bin uniffi-bindgen -- generate --library "$LIB" --language swift --out-dir "$HERE/Generated"
# The C header and module map go in the xcframework; the Swift file in the app.
mkdir -p "$HERE/Generated/headers"
mv "$HERE/Generated/"*.h "$HERE/Generated/headers/"
cat "$HERE/Generated/"*.modulemap > "$HERE/Generated/headers/module.modulemap"
rm "$HERE/Generated/"*.modulemap
xcodebuild -create-xcframework -library "$LIB" -headers "$HERE/Generated/headers" -output "$HERE/IndigoFFI.xcframework" >/dev/null
cd "$HERE" && xcodegen generate >/dev/null
echo "built: $HERE/IndigoDemo.xcodeproj"
