#!/usr/bin/env bash
# Fail before a long build if a runner label selected the wrong CPU.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 amd64|arm64" >&2; exit 2; }
expected=$1
case "$(uname -m)" in
    x86_64|amd64) actual=amd64 ;;
    aarch64|arm64) actual=arm64 ;;
    *) echo "unsupported native architecture: $(uname -m)" >&2; exit 1 ;;
esac
[[ $actual == "$expected" ]] || {
    echo "runner architecture mismatch: expected $expected, found $actual" >&2
    exit 1
}
printf 'native architecture: %s\n' "$actual"
