#!/usr/bin/env bash
# Run from a logged-in macOS GUI session: ./macos/run-status-item-tests.sh
# Uses a temporary synthetic app bundle and the production menu-bar PDF only.
# No account polling, sync, settings writes, screenshots, or installed-app launch.
# Separate from run-tests.sh because an active WindowServer session is required.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_TMP="$(mktemp -d)"
trap 'rm -rf "$TEST_TMP"' EXIT
TEST_APP="$TEST_TMP/Switchboard Status Item Tests.app"
mkdir -p "$TEST_APP/Contents/MacOS" "$TEST_APP/Contents/Resources"

cat > "$TEST_APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>io.github.timomak.switchboard.status-item-tests</string>
  <key>CFBundleExecutable</key><string>status-item-tests</string>
  <key>CFBundleName</key><string>Switchboard Status Item Tests</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSUIElement</key><true/>
</dict></plist>
PLIST
cp "$DIR/assets/switchboard/Switchboard-menubar.pdf" "$TEST_APP/Contents/Resources/"

echo "Compiling status-item GUI regression harness…"
swiftc -O -parse-as-library -D SWIFT_TEST_HARNESS \
  "$DIR/ai-usagebar-menubar.swift" "$DIR/account-switchboard.swift" \
  "$DIR/library-sync-ui.swift" \
  "$DIR/continuation-core.swift" "$DIR/continuation-native.swift" \
  "$DIR/continuation-discovery.swift" "$DIR/continuation-ui.swift" \
  "$DIR/continuation-project.swift" "$DIR/continuation-project-ui.swift" \
  "$DIR/continuation-claude.swift" "$DIR/status-item-tests.swift" \
  -o "$TEST_APP/Contents/MacOS/status-item-tests"

# An external timeout also covers failures before Swift's own watchdog starts.
# subprocess.run kills and reaps only this harness when the 25-second limit hits.
/usr/bin/python3 - "$TEST_APP/Contents/MacOS/status-item-tests" <<'PY'
import subprocess
import sys

try:
    result = subprocess.run([sys.argv[1]], timeout=25, check=False)
except subprocess.TimeoutExpired:
    print("FAIL: status-item harness exceeded its external 25-second bound", file=sys.stderr)
    sys.exit(124)
sys.exit(result.returncode)
PY
