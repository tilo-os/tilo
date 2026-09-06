#!/usr/bin/env bash
# Reject Clippy diagnostics introduced since an exact comparison commit.
#
# A changed package can carry historical lint debt, and a dependency change can
# select otherwise untouched reverse dependencies. Raw `deny` therefore makes
# an unrelated edit responsible for every old diagnostic in those crates. This
# script lints the same package set at the base and at HEAD, then compares stable
# diagnostic fingerprints. Compiler failures are always fatal; Clippy sites may
# disappear, but new sites may not appear.

set -uo pipefail

if [ "$#" -lt 1 ]; then
    echo "usage: $0 <base-revision> [cargo package arguments...]" >&2
    exit 2
fi

base=$1
shift
root=$(cd "$(dirname "$0")/.." && pwd)

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

cleanup() {
    git -C "$root" worktree remove --force "$base_tree" >/dev/null 2>&1 || true
    rm -rf "$scratch"
}
trap cleanup EXIT

git -C "$root" worktree add --quiet --detach "$base_tree" "$base" || exit 2

head_args=("$@")
base_args=()
declare -A base_packages=()
while IFS= read -r package; do
    base_packages["$package"]=1
done < <(
    cargo metadata --manifest-path "$base_tree/Cargo.toml" --locked --offline \
        --no-deps --format-version 1 |
        python3 -c 'import json,sys; print("\n".join(p["name"] for p in json.load(sys.stdin)["packages"]))'
)

base_package_count=0
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
            base_args+=("$argument" "$package")
            base_package_count=$((base_package_count + 1))
        fi
        index=$((index + 2))
        continue
    fi
    base_args+=("$argument")
    index=$((index + 1))
done

target_dir=${CARGO_TARGET_DIR:-$root/target}
base_target_dir="$target_dir/clippy-diff/$base/base"
head_target_dir="$target_dir/clippy-diff/$base/head"

cleanup_builds() {
    # A comparison compiles two complete dependency graphs.  Keeping both
    # graphs until process exit doubles peak disk use, and leaked graphs from
    # interrupted CI runs eventually make an otherwise clean Clippy compile
    # fail without a diagnostic.  Each graph is disposable once its JSON has
    # been captured.
    rm -rf "$base_target_dir" "$head_target_dir"
}

# Extend the earlier scratch/worktree cleanup now that the build directories
# have been resolved.  The paths are rooted below Cargo's target directory and
# include the exact comparison revision, never an unresolved glob.
trap 'cleanup_builds; cleanup' EXIT

run_clippy() {
    local tree=$1
    local output=$2
    local errors=$3
    local build_dir=$4
    shift 4
    (
        cd "$tree" || exit 2
        # Cargo package IDs do not encode a checkout's source revision. Sharing
        # one target directory between the detached base and HEAD can therefore
        # make HEAD link the base revision of a changed workspace dependency,
        # producing bogus missing-item compiler errors instead of a lint diff.
        # Keep artifact graphs separate; sccache still reuses identical compiler
        # work across the two directories.
        CARGO_TARGET_DIR="$build_dir" cargo clippy --locked --all-targets "$@" \
            --message-format=json -- --cap-lints warn >"$output" 2>"$errors"
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

if [ "$base_package_count" -gt 0 ]; then
    if ! run_clippy "$base_tree" "$base_json" "$base_stderr" "$base_target_dir" "${base_args[@]}"; then
        echo "Clippy comparison base did not compile; refusing a false baseline." >&2
        print_compiler_errors "$base_json"
        sed -n '1,160p' "$base_stderr" >&2
        exit 2
    fi
    # The baseline diagnostics are self-contained in base.json.  Reclaim its
    # artifact graph before compiling HEAD so peak usage stays at one graph.
    rm -rf "$base_target_dir"
else
    : >"$base_json"
fi

if ! run_clippy "$root" "$head_json" "$head_stderr" "$head_target_dir" "${head_args[@]}"; then
    echo "HEAD did not compile under Clippy." >&2
    print_compiler_errors "$head_json"
    sed -n '1,160p' "$head_stderr" >&2
    exit 1
fi

python3 - "$base_json" "$head_json" "$base_tree" "$root" <<'PY'
import collections
import json
import pathlib
import sys


def diagnostics(path, tree):
    tree = pathlib.Path(tree).resolve()
    sites = set()
    fingerprints = collections.Counter()
    examples = collections.defaultdict(list)
    with open(path, encoding="utf-8") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if record.get("reason") != "compiler-message":
                continue
            message = record.get("message") or {}
            code = (message.get("code") or {}).get("code", "")
            if not code.startswith("clippy::"):
                continue
            spans = [span for span in message.get("spans", []) if span.get("is_primary")]
            if not spans:
                spans = message.get("spans", [])[:1]
            if not spans:
                fingerprint = (code, "<no-span>", message.get("message", ""), "")
                site = fingerprint
            else:
                span = spans[0]
                file_name = span.get("file_name", "")
                file_path = pathlib.Path(file_name)
                if file_path.is_absolute():
                    try:
                        file_name = file_path.resolve().relative_to(tree).as_posix()
                    except ValueError:
                        continue
                else:
                    file_name = file_path.as_posix().removeprefix("./")
                if file_name.startswith(("vendor/", "../")):
                    continue
                source = "\n".join(part.get("text", "").strip() for part in span.get("text", []))
                fingerprint = (code, file_name, message.get("message", ""), source)
                site = fingerprint + (span.get("line_start"), span.get("column_start"))
            if site in sites:
                continue
            sites.add(site)
            fingerprints[fingerprint] += 1
            examples[fingerprint].append(message.get("rendered") or message.get("message", ""))
    return fingerprints, examples


base, _ = diagnostics(sys.argv[1], sys.argv[3])
head, examples = diagnostics(sys.argv[2], sys.argv[4])
introduced = head - base
if not introduced:
    print(f"Clippy diagnostic diff clean ({sum(head.values())} current, {sum(base.values())} at base)")
    raise SystemExit(0)

print(f"introduced {sum(introduced.values())} Clippy diagnostic site(s):", file=sys.stderr)
for fingerprint, count in introduced.items():
    for rendered in examples[fingerprint][:count]:
        print(rendered.rstrip(), file=sys.stderr)
raise SystemExit(1)
PY
