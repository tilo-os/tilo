#!/usr/bin/env bash
# Instrument-build the hot binaries for profile-guided optimisation.
#
# Produces a `target/pgo-instrumented/` release-shaped build with
# `-C profile-generate=<dir>` bolted onto every rustc invocation. The
# resulting binaries write per-run `.profraw` files to that dir when
# executed — feed them to the next script.
#
# Usage:   scripts/perf/pgo-instrument.sh
# Emits:   target/pgo-profraw/            (empty; populated by runs)
#          target/pgo-instrumented/tilo-session  (and terminal / file-explorer / tilo-install)

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

PROFRAW="$ROOT/target/pgo-profraw"
BUILD="$ROOT/target/pgo-instrumented"
mkdir -p "$PROFRAW" "$BUILD"

# Instrumented binaries have to write .profraw somewhere writable, and
# the default is `$PWD` — force everything into one dir.
export LLVM_PROFILE_FILE="$PROFRAW/tilo-%p-%m.profraw"

# `-Zprofile` is unstable and needs nightly; PGO with `-C
# profile-generate` is stable and covers what we need.
RUSTFLAGS="-C profile-generate=$PROFRAW $(grep -oE '\-C[[:space:]]+[^"]*' .cargo/config.toml | tr '\n' ' ')" \
CARGO_TARGET_DIR="$BUILD" \
run_in_env bash -c "cd $ROOT && cargo build --release \
    -p tilo-session -p terminal -p file-explorer -p install"

echo
echo "Instrumented binaries at $BUILD/release/:"
ls -la "$BUILD/release/tilo-session" "$BUILD/release/terminal" \
       "$BUILD/release/file-explorer" "$BUILD/release/tilo-install" 2>&1 | head
echo
# LLVM_PROFILE_FILE was exported for this script only: a binary run from
# another one needs the same export to land its counters here.
echo "Now exercise them, so each run drops its .profraw files here:"
echo "  export LLVM_PROFILE_FILE=$PROFRAW/tilo-%p-%m.profraw"
echo "Then merge with: scripts/perf/pgo-merge.sh"
