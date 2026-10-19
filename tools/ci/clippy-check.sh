#!/usr/bin/env bash
# Fail on any Clippy diagnostic in the selected workspace packages.
#
#   clippy-check.sh [cargo package arguments...]    # e.g. -p shell -p tilo-ui
#   clippy-check.sh --workspace
#
# The workspace lint table in Cargo.toml is the policy, and every workspace
# member satisfies it, so the gate is absolute: `-D warnings` over the selected
# packages and every workspace member they build on. It does not compare
# against a base revision. `cargo clippy` applies `-D warnings` to workspace
# members only; warnings in registry and vendored dependencies are not ours and
# stay capped.
#
# `--all-features` is deliberately not used: some features link native SDKs
# that exist only in the OS image builder. The release-app workflow separately
# compiles the shipped thin-app feature.

set -uo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root" || exit 2

# Verify the gateway server only when that package itself is selected.
feature_flags=$(python3 tools/ci/package_features.py "$@") || exit 2
read -r -a features <<<"$feature_flags"
# A selected package tested with features its default build lacks is also
# checked as it ships, tests included (package_features.py).
default_builds=$(python3 tools/ci/package_features.py --default-builds "$@") || exit 2
read -r -a default_builds <<<"$default_builds"
set -- "$@" "${features[@]}"

# Let sccache cache Clippy's workspace crates. Cargo runs them as
# `$RUSTC_WRAPPER clippy-driver $RUSTC ...`, and sccache recognises that shape
# only when the compiler argument is literally `rustc`: under rustup Cargo
# otherwise passes the toolchain's full rustc path, which sccache takes for a
# second input file and compiles uncached. The key still covers everything
# Clippy decides on: its arguments (the workspace lint table), sources,
# dependency metadata, and the Cargo.toml, clippy.toml and CLIPPY_ARGS that
# clippy-driver records in its dep-info. A lint failure is never cached.
export RUSTC="${RUSTC:-rustc}"

messages="$root/target/ci/clippy-head.json"
stderr_log="$root/target/ci/clippy-head.stderr"
mkdir -p "$root/target/ci" || exit 2

run_clippy() {
    local source_commit result receipt_name
    source_commit=$(git rev-parse HEAD) || exit 2
    # Report independent packages' failures together instead of requiring
    # another CI iteration for each package left unexamined after an error.
    cargo clippy --locked --all-targets --keep-going "$@" \
        --message-format=json -- -D warnings >"$messages" 2>"$stderr_log"
    result=$?
    receipt_name=clippy-head-inputs.json
    if [ "${SCCACHE_RECACHE:-}" = 1 ]; then
        receipt_name=clippy-head-retry-inputs.json
    fi
    # Observe while the checkout and its dep-info still exist.
    python3 "$root/tools/ci/compiler_inputs.py" \
        --output "$root/target/ci/$receipt_name" --from-messages "$messages" \
        --cargo-exit-code "$result" --expected-commit "$source_commit" -- \
        clippy --locked --all-targets --keep-going "$@" -- -D warnings || true
    return "$result"
}

print_diagnostics() {
    python3 - "$messages" <<'PY'
import json
import sys

seen = set()
with open(sys.argv[1], encoding="utf-8") as stream:
    for line in stream:
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") != "compiler-message":
            continue
        message = record.get("message") or {}
        if message.get("level") not in ("error", "warning"):
            continue
        rendered = (message.get("rendered") or message.get("message", "")).rstrip()
        if rendered in seen:
            continue
        seen.add(rendered)
        print(rendered, file=sys.stderr)
PY
}

has_compiler_diagnostics() {
    python3 - "$messages" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    for line in stream:
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") == "compiler-message" and (record.get("message") or {}).get("level") == "error":
            raise SystemExit(0)
raise SystemExit(1)
PY
}

gate() {
    if run_clippy "$@"; then
        return 0
    fi
    if has_compiler_diagnostics; then
        echo "Clippy rejected this change:" >&2
        print_diagnostics
        return 1
    fi
    echo "Clippy exited without a compiler diagnostic; retrying once without compiler-cache hits." >&2
    tail -80 "$stderr_log" >&2
    if SCCACHE_RECACHE=1 run_clippy "$@"; then
        return 0
    fi
    echo "Clippy failed after the cache-bypassing retry." >&2
    print_diagnostics
    tail -160 "$stderr_log" >&2
    return 1
}

status=0
gate "$@" || status=1
for package in "${default_builds[@]}"; do
    echo "Clippy over $package's default build:" >&2
    gate -p "$package" || status=1
done
exit "$status"
