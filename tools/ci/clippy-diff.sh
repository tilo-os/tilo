#!/usr/bin/env bash
# Reject Clippy diagnostics introduced since an exact comparison commit.
#
# Compare stable Clippy diagnostic fingerprints for the same package set at the
# base and HEAD. HEAD compiler failures are fatal; existing sites may disappear,
# but new sites may not appear. If the base itself does not compile, retain every
# diagnostic it emitted before failing and compare HEAD against that partial
# baseline. Any HEAD diagnostic absent from the partial baseline still fails,
# while a pull request may repair a broken integration branch.

set -uo pipefail

if [ "$#" -lt 1 ]; then
    echo "usage: $0 <base-revision> [cargo package arguments...]" >&2
    exit 2
fi

base=$1
shift
root=$(cd "$(dirname "$0")/../.." && pwd)

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
        cargo clippy --locked --all-targets "$@" \
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

git -C "$root" diff --name-only "$base..HEAD" >"$changed_paths"

python3 - "$base_json" "$head_json" "$base_tree" "$root" "$base_compiled" "$changed_paths" <<'PY'
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
if sys.argv[5] != "true":
    # An early compiler error can prevent Clippy from visiting unchanged files,
    # so a partial baseline cannot prove those diagnostics are new. Continue to
    # reject every diagnostic in a file the branch changed; unchanged base code
    # remains the base branch's responsibility until it compiles again.
    changed = set(pathlib.Path(sys.argv[6]).read_text(encoding="utf-8").splitlines())
    introduced = collections.Counter(
        {
            fingerprint: count
            for fingerprint, count in introduced.items()
            if fingerprint[1] == "<no-span>" or fingerprint[1] in changed
        }
    )
if not introduced:
    print(f"Clippy diagnostic diff clean ({sum(head.values())} current, {sum(base.values())} at base)")
    raise SystemExit(0)

print(f"introduced {sum(introduced.values())} Clippy diagnostic site(s):", file=sys.stderr)
for fingerprint, count in introduced.items():
    for rendered in examples[fingerprint][:count]:
        print(rendered.rstrip(), file=sys.stderr)
raise SystemExit(1)
PY
