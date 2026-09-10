#!/usr/bin/env bash
# Build the ai-usagebar menu bar app (single-file, no Xcode project).
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

command -v swiftc >/dev/null || {
    echo "swiftc not found. Install the Command Line Tools:" >&2
    echo "  xcode-select --install" >&2
    exit 1
}

OUTPUT="${SWITCHBOARD_BUILD_OUTPUT:-$DIR/ai-usagebar-menubar}"
mkdir -p "$(dirname "$OUTPUT")"

echo "› Building (swiftc -O -parse-as-library)…"
swiftc -O -parse-as-library "$DIR/ai-usagebar-menubar.swift" "$DIR/account-switchboard.swift" "$DIR/continuation-core.swift" "$DIR/continuation-native.swift" "$DIR/continuation-discovery.swift" "$DIR/continuation-ui.swift" "$DIR/continuation-project.swift" "$DIR/continuation-project-ui.swift" "$DIR/continuation-claude.swift" -o "$OUTPUT"
echo "✓ Built: $OUTPUT"
echo
echo "Run now:        $OUTPUT &"
echo "Start at login:     $DIR/install-agent.sh"
