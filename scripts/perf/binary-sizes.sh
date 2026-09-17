#!/usr/bin/env bash
# Summarise release binary sizes for the hot binaries.
# Usage:  scripts/perf/binary-sizes.sh [target-dir] > sizes.json
set -euo pipefail

TARGET="${1:-target/release}"
BINS=(shell terminal file-explorer tilo-relay tilo-install tilo-relay-admin fs)

echo '{'
first=1
for b in "${BINS[@]}"; do
    p="$TARGET/$b"
    [ -f "$p" ] || continue
    sz="$(stat --format=%s "$p")"
    [ "$first" = 1 ] || echo ","
    first=0
    printf '  "%s": %d' "$b" "$sz"
done
echo ""
echo "}"
