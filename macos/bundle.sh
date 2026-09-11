#!/usr/bin/env bash
# Build the fork as a self-contained app. Does not install, launch or switch accounts.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
MODE="${1:---switching-enabled}"
case "$MODE" in
  --switching-enabled) PREVIEW=false; SUFFIX="" ;;
  --preview) PREVIEW=true; SUFFIX="-preview" ;;
  *) echo "Usage: $0 [--switching-enabled|--preview]" >&2; exit 2 ;;
esac
[[ $# -le 1 ]] || { echo "Pass exactly one build mode." >&2; exit 2; }
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
<key>CFBundleVersion</key><string>31</string>
<key>CFBundleDevelopmentRegion</key><string>en</string>
<key>CFBundleIconFile</key><string>Switchboard</string>
<key>SwitchboardPreview</key><true/>
<key>LSUIElement</key><true/>
<key>NSAppleEventsUsageDescription</key><string>Restart the selected desktop app when you switch accounts.</string>
</dict></plist>
PLIST
/usr/bin/plutil -replace SwitchboardPreview -bool "$PREVIEW" "$APP/Contents/Info.plist"
/usr/bin/plutil -insert SwitchboardSourceCommit -string "$(git rev-parse HEAD)" "$APP/Contents/Info.plist"
SOURCE_DIRTY=false
[[ -z "$(git status --porcelain)" ]] || SOURCE_DIRTY=true
/usr/bin/plutil -insert SwitchboardSourceDirty -bool "$SOURCE_DIRTY" "$APP/Contents/Info.plist"
/usr/bin/plutil -lint "$APP/Contents/Info.plist"
# Repeated local builds may inherit Finder/resource-fork metadata. Normalize
# only this generated bundle before signing; never touch source or installed apps.
/usr/bin/xattr -cr "$APP"
/usr/bin/codesign --force --deep --sign - "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$REPO/dist/Switchboard-macOS-$(uname -m)$SUFFIX.zip"
printf '\nBuilt (%s): %s\n' "$MODE" "$APP"
