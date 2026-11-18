#!/usr/bin/env bash
# Build "Kamal GUI.app" in target/release/bundle/.
#
#   KGUI_BIN      use this prebuilt binary instead of building (CI passes a
#                 universal binary made with lipo)
#   KGUI_VERSION  version shown in the app's Info.plist (default: Cargo.toml)
set -euo pipefail

cd "$(dirname "$0")/.."

bin="${KGUI_BIN:-}"
if [ -z "$bin" ]; then
  cargo build --release -p kamal-gui
  bin=target/release/kamal-gui
fi

version="${KGUI_VERSION:-$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)}"
# CFBundleVersion must be numeric dot-separated; keep the full string for display.
build_number="$(printf '%s' "$version" | sed 's/[^0-9.].*$//')"

app="target/release/bundle/Kamal GUI.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/kamal-gui"
cp app/assets/icon.icns "$app/Contents/Resources/icon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kamal GUI</string>
  <key>CFBundleDisplayName</key><string>Kamal GUI</string>
  <key>CFBundleIdentifier</key><string>io.github.alexvikman.kamal-gui</string>
  <key>CFBundleVersion</key><string>${build_number}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>kamal-gui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature (no Apple developer identity): required for the bundle to
# launch on Apple Silicon. It is not notarized.
codesign --force --sign - "$app"

echo "Built $app ($version)"
echo "Install with: cp -R \"$app\" /Applications/"
