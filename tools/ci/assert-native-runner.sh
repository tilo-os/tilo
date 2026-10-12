#!/usr/bin/env bash
# Normalize the CI runner identity, then use the shared native host assertion.
set -euo pipefail

expected="${1:-}"
case "$expected" in
  X64) expected=x86_64 ;;
  ARM64) expected=aarch64 ;;
esac
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
exec bash "$repo/os/dev/assert-native-arch.sh" "$expected"
