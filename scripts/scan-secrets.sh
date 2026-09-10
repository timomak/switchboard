#!/usr/bin/env bash
# Gitleaks must already be installed. Never log matched values or upload reports.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
SCANNER="${GITLEAKS_BIN:-gitleaks}"
if [[ "$SCANNER" == */* ]]; then SCANNER="$(cd "$(dirname "$SCANNER")" && pwd)/$(basename "$SCANNER")"; fi
if [[ "${1:-}" == "--history" ]]; then
  exec "$SCANNER" git --config "$PWD/.gitleaks.toml" --log-opts='--all --full-history -m' --redact=100 --no-banner --no-color .
fi
SCAN_TREE="$(mktemp -d)"
trap 'rm -rf "$SCAN_TREE"' EXIT
# Snapshot only versioned files: ignored local credentials and build outputs
# cannot enter the scan artifacts. Added files are included once staged/in CI.
python3 - "$SCAN_TREE" <<'PY'
import pathlib, shutil, subprocess, sys
out = pathlib.Path(sys.argv[1])
for name in subprocess.check_output(['git', 'ls-files', '-z']).split(b'\0'):
    if not name: continue
    src = pathlib.Path(name.decode())
    if src.is_symlink() or not src.is_file(): continue
    dst = out / src
    dst.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(src, dst)
PY
"$SCANNER" dir --config "$PWD/.gitleaks.toml" --redact=100 --no-banner --no-color "$SCAN_TREE"
