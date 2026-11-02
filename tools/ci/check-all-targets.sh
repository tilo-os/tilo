#!/usr/bin/env bash
# Type-check every target of the selected packages, the feature-gated ones too.
#
#   check-all-targets.sh [-p NAME ...] [--receipt PATH]
#
# `--all-targets` leaves out a test, bench or example whose Cargo.toml lists
# `required-features` unless the build enables them, and Cargo says nothing about
# it: code that no longer compiles passes every check that does not name the
# feature. The packages' test features (tools/ci/package_features.py) are enabled
# here, so such a target is type-checked, or the check fails on it.
#
# --receipt writes the compiler-input observation CI uploads beside it
# (tools/ci/compiler_inputs.py).
set -uo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root" || exit 2

receipt=
while [ $# -gt 0 ]; do
    case "$1" in
        --receipt) receipt=${2:?--receipt takes a path}; shift 2 ;;
        *) break ;;
    esac
done

feature_flags=$(python3 tools/ci/package_features.py "$@") || exit 2
read -r -a features <<<"$feature_flags"

arguments=(check --locked --all-targets --keep-going "$@" "${features[@]}")
if [ -n "$receipt" ]; then
    python3 tools/ci/compiler_inputs.py --output "$receipt" -- "${arguments[@]}"
else
    cargo "${arguments[@]}"
fi
