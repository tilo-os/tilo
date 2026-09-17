#!/usr/bin/env bash
# BOLT (Binary Optimization and Layout Tool) post-link optimisation.
# Reorders code by hotness — reduces iTLB + icache pressure. Runs on
# the PGO'd binaries so BOLT and PGO share the same profile signal.
#
# Usage:   scripts/perf/bolt-optimize.sh
# Emits:   target/bolted/{shell,terminal,file-explorer,tilo-install}
#
# Requires: llvm-bolt (Fedora: llvm-bolt package, RPMFusion or ships
# with the current llvm meta package on Fedora 44+).

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
PGO_BUILD="$ROOT/target/pgo-release/release"
OUT="$ROOT/target/bolted"

if [ ! -f "$PROFDATA" ]; then
    echo "no $PROFDATA — run pgo-instrument + pgo-merge + pgo-rebuild first" >&2
    exit 1
fi
if [ ! -f "$PGO_BUILD/shell" ]; then
    echo "no PGO'd binaries at $PGO_BUILD — run pgo-rebuild.sh first" >&2
    exit 1
fi

mkdir -p "$OUT"

# BOLT wants an fdata file, not raw profdata. `llvm-bolt-heatmap` +
# `perf2bolt` are the usual tools; here we use `merge-fdata` on the
# already-emitted branch counters. The exact command shape depends on
# whether the profile was collected via `perf record -e cycles:u -j
# any,u` (perf-mode) or via LBR (--use-lbr).
#
# For repeatability inside CI the guidance is: run this only after
# `perf record` has landed a session.fdata for each binary at
# $ROOT/target/pgo-profraw/<binary>.fdata. The pgo-instrument
# helper is set up to capture those alongside the .profraw files.
for bin in shell terminal file-explorer tilo-install; do
    FDATA="$ROOT/target/pgo-profraw/${bin}.fdata"
    IN="$PGO_BUILD/$bin"
    OUT_BIN="$OUT/$bin"
    if [ ! -f "$FDATA" ]; then
        echo "skip $bin: no $FDATA (was perf record run for this binary?)"
        continue
    fi
    run_in_env bash -c "cd $ROOT && \
        llvm-bolt $IN -o $OUT_BIN -data=$FDATA \
            -reorder-blocks=ext-tsp \
            -reorder-functions=hfsort+ \
            -split-functions -split-all-cold \
            -icf=1 \
            -use-gnu-stack \
            2>&1 | tail -3"
done

echo
ls -la "$OUT/" | head
