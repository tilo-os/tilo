#!/usr/bin/env bash
# Recreate the product-owned screenshots used by Tilo and website documentation.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$repo/target/docs-captures"
build=1

usage() {
  printf 'Usage: %s [--out DIR] [--no-build]\n' "$0"
}

while (($#)); do
  case "$1" in
    --out)
      (($# >= 2)) || { usage >&2; exit 2; }
      out="$2"
      shift 2
      ;;
    --no-build)
      build=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'Unknown argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

out="$(mkdir -p "$out" && cd "$out" && pwd)"
mkdir -p "$out/imager" "$out/onboarding"

if ((build)); then
  cargo build --release -p imager --bin imager --features installation
  cargo build --release -p shell --example onboarding_shots
fi

imager="$repo/target/release/imager"
onboarding="$repo/target/release/examples/onboarding_shots"
[[ -x "$imager" ]] || { printf 'Missing %s; rerun without --no-build\n' "$imager" >&2; exit 1; }
[[ -x "$onboarding" ]] || { printf 'Missing %s; rerun without --no-build\n' "$onboarding" >&2; exit 1; }

"$imager" --shots-dir "$out/imager" --viewport 1180x820
ONBOARDING_SHOTS_OUT="$out/onboarding" "$onboarding"

revision="$(git -C "$repo" rev-parse HEAD)"
dirty=false
[[ -z "$(git -C "$repo" status --porcelain)" ]] || dirty=true
python3 - "$out" "$revision" "$dirty" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
files = {}
for path in sorted(root.glob("**/*.png")):
    files[str(path.relative_to(root))] = hashlib.sha256(path.read_bytes()).hexdigest()
(root / "capture.json").write_text(json.dumps({
    "schemaVersion": 1,
    "tiloRevision": sys.argv[2],
    "tiloDirty": sys.argv[3] == "true",
    "files": files,
}, indent=2, sort_keys=True) + "\n")
PY

printf 'Captured %s documentation frames in %s\n' "$(find "$out" -name '*.png' | wc -l)" "$out"
