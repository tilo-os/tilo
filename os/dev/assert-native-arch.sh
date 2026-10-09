#!/usr/bin/env bash
# Fail before an expensive native build when a runner label is attached to the
# wrong host architecture. Accept release artifact names, normalize uname, and
# print the resolved pair for the CI log.
set -euo pipefail

expected="${1:-}"
case "$expected" in
  x86_64|amd64) expected=x86_64 ;;
  aarch64|arm64) expected=aarch64 ;;
  *) printf 'unsupported expected architecture: %s\n' "$expected" >&2; exit 2 ;;
esac

actual="$(uname -m)"
case "$actual" in
  x86_64|amd64) actual=x86_64 ;;
  aarch64|arm64) actual=aarch64 ;;
  *) printf 'unsupported runner architecture: %s\n' "$actual" >&2; exit 1 ;;
esac

if [ "$actual" != "$expected" ]; then
  printf 'runner architecture mismatch: expected %s, got %s\n' "$expected" "$actual" >&2
  exit 1
fi
printf 'native runner architecture: %s\n' "$actual"
