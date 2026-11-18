#!/usr/bin/env bash
# Build "Kamal GUI.app" in target/release/bundle/.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --release -p kamal-gui

version=$(sed -n 's/^version = "\(.*\)"/\1/p' app/Cargo.toml | head -1)
app="target/release/bundle/Kamal GUI.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/kamal-gui "$app/Contents/MacOS/kamal-gui"
cp app/assets/icon.icns "$app/Contents/Resources/icon.icns"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Kamal GUI</string>
  <key>CFBundleDisplayName</key><string>Kamal GUI</string>
  <key>CFBundleIdentifier</key><string>io.github.alexvikman.kamal-gui</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>CFBundleShortVersionString</key><string>${version}</string>
  <key>CFBundleExecutable</key><string>kamal-gui</string>
  <key>CFBundleIconFile</key><string>icon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
echo "Built $app"
echo "Install with: cp -R \"$app\" /Applications/"
