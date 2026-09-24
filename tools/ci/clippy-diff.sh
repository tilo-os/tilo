#!/usr/bin/env bash
# Reject Clippy diagnostics introduced since an exact comparison commit.
#
# Compare stable Clippy diagnostic fingerprints for the same package set at the
# base and HEAD. HEAD compiler failures are fatal; existing sites may disappear,
# but new sites may not appear. If the base itself does not compile, retain every
# diagnostic it emitted before failing and compare HEAD against that partial
# baseline. Diagnostics remain enforceable only for packages that completed at
# least one base artifact; dependent packages the compiler never reached cannot
# provide evidence that a HEAD diagnostic is new. Fingerprinting and the
# threshold-lint normalization live in tools/ci/clippy_diagnostics.py.
#
# Cached base: when CLIPPY_BASE_CACHE names a file recorded by
# `--export-base` for exactly this base commit, and CLIPPY_BASE_CACHE_KEY
# matches the key it was recorded under (toolchain, builder image, flags and
# these helpers), the base diagnostics come from that workspace-wide record,
# restricted to the workspace members `cargo clippy` lints for the selected
# packages. No base checkout or base compilation happens then. A clean result
# is final. A cached comparison that reports new sites is confirmed against a
# fresh base checkout before failing, so a cached base can never fail a change
# that the uncached comparison accepts. Any missing, stale or mismatched cache
# falls back to the fresh base checkout.
#
#   clippy-diff.sh <base-revision> [cargo package arguments...]
#   clippy-diff.sh --export-base <output> <cache-key>
#
# `--export-base` lints the complete workspace at the checked-out commit and
# records its normalized diagnostics for later comparisons against it.

set -uo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
diagnostics="$root/tools/ci/clippy_diagnostics.py"

# Each checkout owns Cargo's default target directory. An explicit
# CARGO_TARGET_DIR changes sccache keys even when it names the default path.
# Never share artifact graphs between the detached base and the working tree.
unset CARGO_TARGET_DIR

run_clippy() {
    local tree=$1
    local output=$2
    local errors=$3
    shift 3
    (
        cd "$tree" || exit 2
        # Cargo package IDs do not encode a checkout's source revision. Sharing
        # one target directory between the detached base and HEAD can therefore
        # make HEAD link the base revision of a changed workspace dependency,
        # producing bogus missing-item compiler errors instead of a lint diff.
        # Keep artifact graphs separate; sccache still reuses identical compiler
        # work across the two directories.
        local source_commit result receipt_name
        source_commit=$(git rev-parse HEAD) || exit 2
        cargo clippy --locked --all-targets "$@" \
            --message-format=json -- --cap-lints warn >"$output" 2>"$errors"
        result=$?
        receipt_name=clippy-base-inputs.json
        [ "$tree" != "$root" ] || receipt_name=clippy-head-inputs.json
        if [ "$tree" = "$root" ] && [ "${SCCACHE_RECACHE:-}" = 1 ]; then
            receipt_name=clippy-head-retry-inputs.json
        fi
        receipt_name=${CLIPPY_RECEIPT_NAME:-$receipt_name}
        # Observe while this checkout and its dep-info still exist. Use the HEAD
        # observer for both revisions; the comparison base may predate it.
        python3 "$root/tools/ci/compiler_inputs.py" \
            --output "$root/target/ci/$receipt_name" --from-messages "$output" \
            --cargo-exit-code "$result" --expected-commit "$source_commit" -- \
            clippy --locked --all-targets "$@" -- --cap-lints warn || true
        exit "$result"
    )
}

print_compiler_errors() {
    python3 - "$1" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    for line in stream:
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        if record.get("reason") != "compiler-message":
            continue
        message = record.get("message") or {}
        if message.get("level") == "error":
            print((message.get("rendered") or message.get("message", "")).rstrip(), file=sys.stderr)
PY
}

has_compiler_errors() {
    python3 - "$1" <<'PY'
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

if [ "${1:-}" = "--export-base" ]; then
    if [ "$#" -ne 3 ]; then
        echo "usage: $0 --export-base <output> <cache-key>" >&2
        exit 2
    fi
    output=$2
    key=$3
    if ! git -C "$root" diff --quiet HEAD --; then
        echo "Refusing to record a Clippy base cache from a modified working tree." >&2
        exit 2
    fi
    commit=$(git -C "$root" rev-parse HEAD) || exit 2
    scratch=$(mktemp -d) || exit 2
    trap 'rm -rf "$scratch"' EXIT
    host=$(rustc -vV | sed -n 's/^host: //p')
    [ -n "$host" ] || exit 2
    cargo metadata --manifest-path "$root/Cargo.toml" --locked --format-version 1 \
        --filter-platform "$host" >"$scratch/metadata.json" || exit 2
    if ! CLIPPY_RECEIPT_NAME=clippy-workspace-inputs.json \
        run_clippy "$root" "$scratch/workspace.json" "$scratch/workspace.stderr" --workspace; then
        echo "Workspace Clippy did not complete; no base cache recorded for $commit." >&2
        print_compiler_errors "$scratch/workspace.json"
        tail -160 "$scratch/workspace.stderr" >&2
        exit 1
    fi
    python3 "$diagnostics" export --messages "$scratch/workspace.json" --tree "$root" \
        --metadata "$scratch/metadata.json" --commit "$commit" --key "$key" --output "$output"
    exit $?
fi

if [ "$#" -lt 1 ]; then
    echo "usage: $0 <base-revision> [cargo package arguments...]" >&2
    exit 2
fi

base=$1
shift

if ! git -C "$root" cat-file -e "$base^{commit}" 2>/dev/null; then
    echo "Clippy comparison base is not available: $base" >&2
    exit 2
fi
base=$(git -C "$root" rev-parse "$base^{commit}")

scratch=$(mktemp -d) || exit 2
base_tree="$scratch/base"
base_json="$scratch/base.json"
head_json="$scratch/head.json"
base_stderr="$scratch/base.stderr"
head_stderr="$scratch/head.stderr"
changed_paths="$scratch/changed-paths"
base_metadata="$scratch/base-metadata.json"
head_metadata="$scratch/head-metadata.json"

cleanup() {
    git -C "$root" worktree remove --force "$base_tree" >/dev/null 2>&1 || true
    rm -rf "$scratch"
}
trap cleanup EXIT

prepare_base_checkout() {
    git -C "$root" worktree add --quiet --detach "$base_tree" "$base" || exit 2
    cargo metadata --manifest-path "$base_tree/Cargo.toml" --locked --offline \
        --no-deps --format-version 1 >"$base_metadata" || exit 2
}

git -C "$root" diff --name-status -M "$base..HEAD" >"$changed_paths"
cargo metadata --manifest-path "$root/Cargo.toml" --locked --offline \
    --no-deps --format-version 1 >"$head_metadata" || exit 2

head_args=("$@")

use_cache=false
if [ -n "${CLIPPY_BASE_CACHE:-}" ]; then
    cache_reason=
    index=0
    while [ "$index" -lt "${#head_args[@]}" ]; do
        case "${head_args[$index]}" in
            -p|--package) index=$((index + 2)) ;;
            *) cache_reason="arguments other than -p/--package"; break ;;
        esac
    done
    if [ -z "$cache_reason" ] && [ ! -f "$CLIPPY_BASE_CACHE" ]; then
        cache_reason="no cache recorded for this base"
    fi
    if [ -z "$cache_reason" ] && cache_reason=$(python3 "$diagnostics" cache-check \
        --cache "$CLIPPY_BASE_CACHE" --commit "$base" --key "${CLIPPY_BASE_CACHE_KEY:-}") &&
        python3 "$diagnostics" cache-metadata --cache "$CLIPPY_BASE_CACHE" --commit "$base" \
            --key "${CLIPPY_BASE_CACHE_KEY:-}" --tree "$base_tree" >"$base_metadata"; then
        use_cache=true
        echo "Using cached Clippy diagnostics for base $base."
    else
        echo "Cached Clippy base unusable (${cache_reason:-invalid}); linting a fresh base checkout."
    fi
fi
$use_cache || prepare_base_checkout

# Translate the HEAD package arguments into the base revision's package names.
derive_base_args() {
    base_args=()
    base_package_names=()
    base_package_count=0
    local -A base_packages=()
    local -A renamed_base_packages=()
    local package head_package base_package argument next index
    while IFS= read -r package; do
        base_packages["$package"]=1
    done < <(
        python3 -c 'import json,sys; print("\n".join(p["name"] for p in json.load(open(sys.argv[1]))["packages"]))' \
            "$base_metadata"
    )
    while IFS=$'\t' read -r head_package base_package; do
        renamed_base_packages["$head_package"]=$base_package
    done < <(python3 - "$base_metadata" "$head_metadata" "$base_tree" "$root" "$changed_paths" <<'PY'
import json
import pathlib
import sys

base = json.load(open(sys.argv[1], encoding="utf-8"))
head = json.load(open(sys.argv[2], encoding="utf-8"))
base_root = pathlib.Path(sys.argv[3]).resolve()
head_root = pathlib.Path(sys.argv[4]).resolve()
renames = {}
for line in pathlib.Path(sys.argv[5]).read_text(encoding="utf-8").splitlines():
    fields = line.split("\t")
    if fields[0].startswith("R") and len(fields) == 3:
        renames[fields[1]] = fields[2]

base_by_head_manifest = {}
for package in base["packages"]:
    relative = pathlib.Path(package["manifest_path"]).resolve().relative_to(base_root).as_posix()
    base_by_head_manifest[renames.get(relative, relative)] = package["name"]
for package in head["packages"]:
    relative = pathlib.Path(package["manifest_path"]).resolve().relative_to(head_root).as_posix()
    base_name = base_by_head_manifest.get(relative)
    if base_name and base_name != package["name"]:
        print(f'{package["name"]}\t{base_name}')
PY
    )

    index=0
    while [ "$index" -lt "${#head_args[@]}" ]; do
        argument=${head_args[$index]}
        if [ "$argument" = "-p" ] || [ "$argument" = "--package" ]; then
            next=$((index + 1))
            if [ "$next" -ge "${#head_args[@]}" ]; then
                echo "$argument requires a package name" >&2
                exit 2
            fi
            package=${head_args[$next]}
            if [ -n "${base_packages[$package]:-}" ]; then
                base_package=$package
            else
                base_package=${renamed_base_packages[$package]:-}
            fi
            if [ -n "$base_package" ]; then
                base_args+=("$argument" "$base_package")
                base_package_names+=(--base-package "$base_package")
                base_package_count=$((base_package_count + 1))
            fi
            index=$((index + 2))
            continue
        fi
        base_args+=("$argument")
        index=$((index + 1))
    done
}
derive_base_args

if ! run_clippy "$root" "$head_json" "$head_stderr" "${head_args[@]}"; then
    if has_compiler_errors "$head_json"; then
        echo "HEAD did not compile under Clippy." >&2
        print_compiler_errors "$head_json"
        tail -160 "$head_stderr" >&2
        exit 1
    fi
    echo "HEAD Clippy exited without a compiler diagnostic; retrying failed compilation once without compiler-cache hits." >&2
    tail -80 "$head_stderr" >&2
    if ! SCCACHE_RECACHE=1 run_clippy "$root" "$head_json" "$head_stderr" "${head_args[@]}"; then
        echo "HEAD did not compile under Clippy after the cache-bypassing retry." >&2
        print_compiler_errors "$head_json"
        tail -160 "$head_stderr" >&2
        exit 1
    fi
fi
# Preserve the working tree target for subsequent checks and local development.

if $use_cache; then
    if python3 "$diagnostics" compare --head-messages "$head_json" --head-tree "$root" \
        --changed "$changed_paths" --base-cache "$CLIPPY_BASE_CACHE" --base-commit "$base" \
        --base-key "$CLIPPY_BASE_CACHE_KEY" "${base_package_names[@]}" 2>"$scratch/cached-compare.stderr"; then
        exit 0
    fi
    # Workspace-wide lint runs can see code a package subset does not build
    # (for example `cfg(not(feature = ...))` branches), so only a fresh base
    # may reject the change.
    echo "HEAD differs from the cached base diagnostics; confirming against a fresh base checkout." >&2
    prepare_base_checkout
    derive_base_args
fi

base_compiled=true
if [ "$base_package_count" -gt 0 ]; then
    if ! run_clippy "$base_tree" "$base_json" "$base_stderr" "${base_args[@]}"; then
        base_compiled=false
        echo "Clippy comparison base did not compile; using its partial diagnostic baseline." >&2
        print_compiler_errors "$base_json"
        sed -n '1,160p' "$base_stderr" >&2
    fi
    # The EXIT trap removes only the disposable base worktree and its target.
else
    : >"$base_json"
fi

python3 "$diagnostics" compare --head-messages "$head_json" --head-tree "$root" \
    --changed "$changed_paths" --base-messages "$base_json" --base-tree "$base_tree" \
    --base-compiled "$base_compiled" --base-metadata "$base_metadata"
