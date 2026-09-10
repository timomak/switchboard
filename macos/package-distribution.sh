#!/usr/bin/env bash
# Sign a separate candidate in the requested build mode. Never install or launch.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
: "${SWITCHBOARD_SIGNING_IDENTITY:?Set the existing Developer ID Application identity name or SHA-1.}"
cd "$REPO"
[[ -z "$(git status --porcelain --untracked-files=normal)" ]] || {
  echo "Commit source changes before packaging so the signed candidate identifies exact source." >&2; exit 1;
}
./macos/bundle.sh "$@"
PACKAGE_ROOT="${SWITCHBOARD_DISTRIBUTION_ROOT:-$REPO/dist/distribution}"
mkdir -p "$PACKAGE_ROOT"
PACKAGE_DIR="$(mktemp -d "$PACKAGE_ROOT/package.XXXXXX")"
git rev-parse HEAD > "$PACKAGE_DIR/source-commit.txt"
APP="$PACKAGE_DIR/Switchboard.app"
/usr/bin/ditto "$REPO/dist/Switchboard.app" "$APP"
/usr/bin/xattr -cr "$APP"
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
