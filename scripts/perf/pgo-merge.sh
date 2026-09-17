#!/usr/bin/env bash
# Merge every .profraw dropped by instrumented runs into the single
# .profdata file the second-phase build consumes.
#
# Commits the merged file into os/pgo/tilo.profdata — that's what makes
# the whole PGO pipeline reproducible under the frozen RPM repo: the
# same source + same profile bytes = the same compiled output.
#
# Usage:   scripts/perf/pgo-merge.sh
# Emits:   os/pgo/tilo.profdata (ready to commit)

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
OUT="$ROOT/os/pgo/tilo.profdata"
mkdir -p "$(dirname "$OUT")"

if ! compgen -G "$PROFRAW/*.profraw" > /dev/null; then
    echo "no .profraw files in $PROFRAW/" >&2
    echo "run scripts/perf/pgo-instrument.sh then exercise the binaries first" >&2
    exit 1
fi

# llvm-profdata ships with clang. Fedora packages it as `llvm`.
run_in_env bash -c "cd $ROOT && \
    llvm-profdata merge \
        --output=$OUT \
        $PROFRAW/*.profraw"

echo
echo "Merged $(ls $PROFRAW/*.profraw | wc -l) .profraw files into:"
echo "    $OUT"
ls -la "$OUT"
echo
echo "Next: scripts/perf/pgo-rebuild.sh"
