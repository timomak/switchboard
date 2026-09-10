#!/usr/bin/env bash
# Uses an existing Keychain credential profile; never accepts raw secret arguments.
# Re-run with the same directory to resume an existing submission, not upload twice.
set -euo pipefail
: "${SWITCHBOARD_NOTARY_PROFILE:?Set the notarization Keychain profile name.}"
PACKAGE_DIR="${1:?Pass the package directory printed by package-distribution.sh.}"
PACKAGE_DIR="$(cd "$PACKAGE_DIR" && pwd)"
APP="$PACKAGE_DIR/Switchboard.app"
[[ -f "$APP/Contents/Info.plist" && -f "$PACKAGE_DIR/submission.zip" ]] || { echo "Missing signed candidate." >&2; exit 1; }
[[ "$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$APP/Contents/Info.plist")" == "io.github.timomak.switchboard" ]] || { echo "Not a Switchboard bundle." >&2; exit 1; }
/usr/bin/codesign --verify --deep --strict "$APP"
# Bind any resumption/stapling to the exact signed app and uploaded archive.
EXPECTED_HASH="$(cat "$PACKAGE_DIR/submission.sha256")"
ACTUAL_HASH="$(/usr/bin/shasum -a 256 "$PACKAGE_DIR/submission.zip" | awk '{print $1}')"
APP_HASH="$(/usr/bin/codesign -dv --verbose=4 "$APP" 2>&1 | sed -n 's/^CDHash=//p')"
[[ -n "$APP_HASH" && "$APP_HASH" == "$(cat "$PACKAGE_DIR/app.cdhash")" && "$ACTUAL_HASH" == "$EXPECTED_HASH" ]] || {
  echo "Candidate changed; create a fresh signed package." >&2; exit 1;
}
if [[ ! -f "$PACKAGE_DIR/submission.json" ]]; then
  [[ ! -f "$PACKAGE_DIR/submission-attempted" ]] || {
    echo "An earlier upload has an uncertain result. Reconcile notarytool history before retrying; no duplicate upload attempted." >&2; exit 1;
  }
  touch "$PACKAGE_DIR/submission-attempted"
  /usr/bin/xcrun notarytool submit "$PACKAGE_DIR/submission.zip" \
    --keychain-profile "$SWITCHBOARD_NOTARY_PROFILE" --no-wait --output-format json > "$PACKAGE_DIR/submission-response.tmp"
  python3 -c 'import json,sys,uuid; uuid.UUID(json.load(open(sys.argv[1]))["id"])' "$PACKAGE_DIR/submission-response.tmp"
  mv "$PACKAGE_DIR/submission-response.tmp" "$PACKAGE_DIR/submission.json"
fi
SUBMISSION_ID="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$PACKAGE_DIR/submission.json")"
# Timeout preserves the accepted upload ID. Re-run this script to continue waiting.
/usr/bin/xcrun notarytool wait "$SUBMISSION_ID" --keychain-profile "$SWITCHBOARD_NOTARY_PROFILE" --timeout 60s
/usr/bin/xcrun notarytool info "$SUBMISSION_ID" --keychain-profile "$SWITCHBOARD_NOTARY_PROFILE" \
  --output-format json > "$PACKAGE_DIR/notarization.json"
python3 - "$PACKAGE_DIR/notarization.json" <<'PY'
import json, sys
if json.load(open(sys.argv[1]))['status'] != 'Accepted':
    raise SystemExit('Apple has not accepted this candidate. Inspect the notarization log; do not distribute it.')
PY
/usr/bin/xcrun stapler staple "$APP"
/usr/bin/xcrun stapler validate "$APP"
/usr/bin/codesign --verify --deep --strict "$APP"
/usr/sbin/spctl --assess --type execute --verbose=2 "$APP"
VERSION="$(/usr/libexec/PlistBuddy -c 'Print CFBundleShortVersionString' "$APP/Contents/Info.plist")"
BUILD="$(/usr/libexec/PlistBuddy -c 'Print CFBundleVersion' "$APP/Contents/Info.plist")"
SUFFIX=""
if [[ "$(/usr/libexec/PlistBuddy -c 'Print SwitchboardPreview' "$APP/Contents/Info.plist")" == "true" ]]; then SUFFIX="-preview"; fi
ARCHIVE="Switchboard-$VERSION-$BUILD-macOS-$(uname -m)$SUFFIX.zip"
/usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP" "$PACKAGE_DIR/$ARCHIVE"
(cd "$PACKAGE_DIR" && /usr/bin/shasum -a 256 "$ARCHIVE" > SHA256SUMS)
printf 'Notarized archive ready for review: %s/%s\n' "$PACKAGE_DIR" "$ARCHIVE"
