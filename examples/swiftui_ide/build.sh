#!/usr/bin/env bash
# Build the SwiftUI rustcc IDE: fork-Rust engine staticlib + SwiftUI
# front-end linked against it, wrapped in a .app bundle.
#
#   ./build.sh         # build engine + app bundle
#   ./build.sh run     # build, then launch the app
#   ./build.sh test    # engine self-test only (no Swift toolchain)
#
# Requires: the fork rustc (RUSTC= env or the default migration path)
# and, for the app, swiftc + the macOS SDK.
set -euo pipefail
cd "$(dirname "$0")"

RUSTC="${RUSTC:-$HOME/rust-1.96-migration/build/host/stage1/bin/rustc}"
MODE="${1:-build}"

echo "==> engine: cargo +nightly ($MODE) via fork rustc"
if [ "$MODE" = "test" ]; then
    RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly test
    exit 0
fi
RUSTC="$RUSTC" RUSTC_BOOTSTRAP=1 cargo +nightly build --release

if ! command -v swiftc >/dev/null 2>&1; then
    echo "swiftc not found — engine built (target/release/libswiftui_ide.a);"
    echo "the SwiftUI app needs swiftc + the macOS SDK to link."
    exit 0
fi

OUT=build
APP="$OUT/RustccIDE.app"
echo "==> app: swiftc link against libswiftui_ide.a"
rm -rf "$OUT"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

swiftc -O -parse-as-library \
    swift/*.swift \
    -L target/release -lswiftui_ide \
    -o "$APP/Contents/MacOS/RustccIDE"

# --- app icon: render the source PNG, build the iconset, make .icns ---
echo "==> icon: render + iconutil"
swift icon/make_icon.swift "$OUT/icon_1024.png"
ICONSET="$OUT/RustccIDE.iconset"
mkdir -p "$ICONSET"
for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$OUT/icon_1024.png" --out "$ICONSET/icon_${s}x${s}.png" >/dev/null
    d=$((s * 2))
    sips -z "$d" "$d" "$OUT/icon_1024.png" --out "$ICONSET/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/RustccIDE.icns"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key><string>RustccIDE</string>
  <key>CFBundleIdentifier</key><string>com.rustcc.swiftui-ide</string>
  <key>CFBundleName</key><string>rustcc IDE</string>
  <key>CFBundleIconFile</key><string>RustccIDE</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSPrincipalClass</key><string>NSApplication</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

echo "built $APP"
if [ "$MODE" = "run" ]; then
    echo "==> launching"
    open "$APP"
fi
