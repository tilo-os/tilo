#!/usr/bin/env bash
# Second-phase build: consume the merged .profdata to guide codegen.
#
# Reads os/pgo/tilo.profdata (produced by scripts/perf/pgo-merge.sh) and
# builds the release-shaped binaries with `-C profile-use=<file>`. The
# output lands at target/pgo-release/release/, out of the way of a
# normal `cargo build --release`.
#
# Usage:   scripts/perf/pgo-rebuild.sh
# Emits:   target/pgo-release/release/{shell,terminal,file-explorer,tilo-install}

set -euo pipefail

DISTROBOX="${TILO_DISTROBOX:-}"
run_in_env() {
    if [ -n "$DISTROBOX" ]; then
        distrobox enter "$DISTROBOX" -- "$@"
    else
        "$@"
    fi
}

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

PROFDATA="$ROOT/os/pgo/tilo.profdata"
BUILD="$ROOT/target/pgo-release"

if [ ! -f "$PROFDATA" ]; then
    echo "no $PROFDATA — run pgo-instrument.sh + pgo-merge.sh first" >&2
    exit 1
fi

# `-C profile-use=<file>` reads the merged .profdata and shapes codegen
# accordingly. Combined with the workspace's fat LTO + single codegen
# unit, this squeezes another 5–20% out of hot loops.
CARGO_TARGET_DIR="$BUILD" \
RUSTFLAGS="-C profile-use=$PROFDATA $(grep -oE '\-C[[:space:]]+[^"]*' .cargo/config.toml | tr '\n' ' ')" \
run_in_env bash -c "cd $ROOT && cargo build --release \
    -p shell -p terminal -p file-explorer -p install"

echo
ls -la "$BUILD/release/shell" "$BUILD/release/terminal" \
       "$BUILD/release/file-explorer" "$BUILD/release/tilo-install"
echo
echo "PGO'd binaries. Ready for BOLT (scripts/perf/bolt-optimize.sh)."
