#!/usr/bin/env bash
# Builds the Apple and Android native libraries into dist/ (run on macOS).
#
#   dist/ios/OfflineFirstCore.xcframework   device (arm64) + simulator (arm64, x86_64)
#   dist/macos/liboffline_first_core.dylib  universal (arm64, x86_64)
#   dist/android/<abi>/liboffline_first_core.so
#
# Requires: rustup targets aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios
# aarch64-apple-darwin x86_64-apple-darwin and the four Android targets, Xcode,
# cargo-ndk and ANDROID_NDK_HOME.
set -euo pipefail

cd "$(dirname "$0")/.."
DIST="${DIST:-dist}"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-13.0}"
export MACOSX_DEPLOYMENT_TARGET="${MACOSX_DEPLOYMENT_TARGET:-10.15}"
LIB=liboffline_first_core.dylib
rm -rf "$DIST" && mkdir -p "$DIST/ios" "$DIST/macos" "$DIST/android"

for target in aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios aarch64-apple-darwin x86_64-apple-darwin; do
  cargo build --release --target "$target"
done

# iOS: one dynamic framework per platform, combined into an xcframework.
make_framework() { # <output dir> <dylib>
  local framework="$1/OfflineFirstCore.framework"
  mkdir -p "$framework"
  cp "$2" "$framework/OfflineFirstCore"
  install_name_tool -id @rpath/OfflineFirstCore.framework/OfflineFirstCore "$framework/OfflineFirstCore"
  cat > "$framework/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>OfflineFirstCore</string>
  <key>CFBundleIdentifier</key><string>com.jhonacode.offlinefirstcore</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>OfflineFirstCore</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>$(cargo pkgid | sed 's/.*[#@]//')</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>MinimumOSVersion</key><string>${IPHONEOS_DEPLOYMENT_TARGET}</string>
</dict>
</plist>
PLIST
}
TMP="$(mktemp -d)"
make_framework "$TMP/device" "target/aarch64-apple-ios/release/$LIB"
lipo -create "target/aarch64-apple-ios-sim/release/$LIB" "target/x86_64-apple-ios/release/$LIB" -output "$TMP/sim.dylib"
make_framework "$TMP/simulator" "$TMP/sim.dylib"
xcodebuild -create-xcframework \
  -framework "$TMP/device/OfflineFirstCore.framework" \
  -framework "$TMP/simulator/OfflineFirstCore.framework" \
  -output "$DIST/ios/OfflineFirstCore.xcframework" >/dev/null

# macOS: one universal dylib.
lipo -create "target/aarch64-apple-darwin/release/$LIB" "target/x86_64-apple-darwin/release/$LIB" \
  -output "$DIST/macos/$LIB"
install_name_tool -id "@rpath/$LIB" "$DIST/macos/$LIB"

# Android: 16 KB page aligned (see .cargo/config.toml).
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86 -t x86_64 -o "$DIST/android" build --release

rm -rf "$TMP"
find "$DIST" -type f | sort
