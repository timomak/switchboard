#!/usr/bin/env bash
# Sign a separate candidate. Never install, launch, or disable preview guards.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${SWITCHBOARD_SIGNING_IDENTITY:?Set the existing Developer ID Application identity name or SHA-1.}"
cd "$REPO"
./macos/bundle.sh
mkdir -p "$REPO/dist/distribution"
PACKAGE_DIR="$(mktemp -d "$REPO/dist/distribution/package.XXXXXX")"
APP="$PACKAGE_DIR/Switchboard.app"
/usr/bin/ditto "$REPO/dist/Switchboard.app" "$APP"
IDENTIFIER="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$APP/Contents/Info.plist")"
for binary in ai-usagebar ai-usagebar-tui; do
  /usr/bin/codesign --force --sign "$SWITCHBOARD_SIGNING_IDENTITY" --timestamp --options runtime \
    --identifier "$IDENTIFIER.$binary" --entitlements macos/distribution.entitlements \
    "$APP/Contents/Resources/bin/$binary"
done
/usr/bin/codesign --force --sign "$SWITCHBOARD_SIGNING_IDENTITY" --timestamp --options runtime \
  --entitlements macos/distribution.entitlements "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
for code in "$APP" "$APP/Contents/Resources/bin/ai-usagebar" "$APP/Contents/Resources/bin/ai-usagebar-tui"; do
  DETAILS="$(/usr/bin/codesign -dv --verbose=4 "$code" 2>&1)"
  [[ "$DETAILS" == *"Authority=Developer ID Application:"* && "$DETAILS" == *"runtime"* && "$DETAILS" == *"Timestamp="* ]] || {
    echo "Distribution signature validation failed." >&2; exit 1;
  }
done
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$PACKAGE_DIR/submission.zip"
/usr/bin/shasum -a 256 "$PACKAGE_DIR/submission.zip" | awk '{print $1}' > "$PACKAGE_DIR/submission.sha256"
/usr/bin/codesign -dv --verbose=4 "$APP" 2>&1 | sed -n 's/^CDHash=//p' > "$PACKAGE_DIR/app.cdhash"
printf 'Signed candidate (not yet notarized): %s\n' "$PACKAGE_DIR"
