#!/usr/bin/env bash
# Build the fork as a self-contained app. Does not install, launch or switch accounts.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
cargo build --release --locked --bins
SWITCHBOARD_BUILD_OUTPUT="$REPO/dist/build/ai-usagebar-menubar" ./macos/build.sh
APP="$REPO/dist/Switchboard.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources/bin"
mkdir -p "$APP/Contents/Resources/provider-icons"
install -m 644 macos/assets/provider-icons/*.png "$APP/Contents/Resources/provider-icons/"
install -m 755 "$REPO/dist/build/ai-usagebar-menubar" "$APP/Contents/MacOS/ai-usagebar-menubar"
install -m 755 "${CARGO_TARGET_DIR:-$REPO/target}/release/ai-usagebar" "${CARGO_TARGET_DIR:-$REPO/target}/release/ai-usagebar-tui" "$APP/Contents/Resources/bin/"
install -m 644 macos/assets/switchboard/Switchboard.icns macos/assets/switchboard/Switchboard-menubar.pdf macos/assets/switchboard/Switchboard-menubar.svg "$APP/Contents/Resources/"
install -m 644 LICENSE THIRD_PARTY_NOTICES.md "$APP/Contents/Resources/"
cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>io.github.timomak.switchboard</string>
<key>CFBundleName</key><string>Switchboard</string>
<key>CFBundleDisplayName</key><string>Switchboard</string>
<key>CFBundleExecutable</key><string>ai-usagebar-menubar</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>1.12.0</string>
<key>CFBundleVersion</key><string>17</string>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleIconFile</key><string>Switchboard</string>
<key>SwitchboardPreview</key><true/>
<key>LSUIElement</key><true/>
<key>NSAppleEventsUsageDescription</key><string>Restart the selected desktop app when you switch accounts.</string>
</dict></plist>
PLIST
/usr/bin/plutil -lint "$APP/Contents/Info.plist"
/usr/bin/codesign --force --deep --sign - "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$REPO/dist/Switchboard-macOS-$(uname -m).zip"
printf '\nBuilt: %s\n' "$APP"
