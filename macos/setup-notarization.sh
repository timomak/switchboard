#!/usr/bin/env bash
# Run interactively. Password entry is handled by Apple's secure prompt.
# Do not add --password or put the password in this script or shell history.
set -euo pipefail
read -r -p 'Apple Account email: ' SWITCHBOARD_APPLE_ACCOUNT
read -r -p 'Apple Developer Team ID: ' SWITCHBOARD_APPLE_TEAM
exec /usr/bin/xcrun notarytool store-credentials Switchboard-notarization \
  --apple-id "$SWITCHBOARD_APPLE_ACCOUNT" --team-id "$SWITCHBOARD_APPLE_TEAM"
