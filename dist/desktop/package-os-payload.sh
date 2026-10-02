#!/usr/bin/env bash
# Package the native desktop-session and runtime binaries for immutable OS reuse.
set -euo pipefail

[[ $# == 3 ]] || {
  echo "usage: $0 BIN_DIR x86_64|aarch64 OUTPUT_DIR" >&2
  exit 2
}
bin_dir=$(realpath "$1")
architecture=$2
out=$(realpath -m "$3")
case "$architecture" in x86_64|aarch64) ;; *) exit 2 ;; esac

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source_epoch=${SOURCE_DATE_EPOCH:-0}
[[ $source_epoch =~ ^[0-9]+$ ]] || {
    printf 'SOURCE_DATE_EPOCH must be a non-negative integer\n' >&2
    exit 2
}
source_revision=${GITHUB_SHA:-$(git -C "$repo_root" rev-parse HEAD)}
work=$(mktemp -d)
cleanup() { rm -rf -- "$work"; }
trap cleanup EXIT

for binary in shell tilo-launcher tilo-runtime-mcp tilo-unlock-helper; do
  [[ -x $bin_dir/$binary ]] || {
    printf 'missing desktop binary: %s\n' "$bin_dir/$binary" >&2
    exit 1
  }
  install -m 0755 "$bin_dir/$binary" "$work/$binary"
done

python3 - "$work/manifest.json" "$architecture" "$source_revision" <<'PY'
import json
import sys

json.dump(
    {"format": 1, "architecture": sys.argv[2], "source": sys.argv[3]},
    open(sys.argv[1], "w", encoding="utf-8"),
    indent=2,
    sort_keys=True,
)
PY
(cd "$work" && sha256sum shell tilo-launcher tilo-runtime-mcp tilo-unlock-helper manifest.json > SHA256SUMS)
mkdir -p "$out"
bundle="$out/tilo-desktop-os-$architecture.tar.gz"
tar --sort=name --mtime="@$source_epoch" --owner=0 --group=0 --numeric-owner \
  -C "$work" -czf "$bundle" \
  shell tilo-launcher tilo-runtime-mcp tilo-unlock-helper manifest.json SHA256SUMS
(cd "$out" && sha256sum "${bundle##*/}" > "${bundle##*/}.sha256")
