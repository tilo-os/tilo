#!/usr/bin/env bash
# Reject Clippy diagnostics introduced since an exact comparison commit.
#
# Compare stable Clippy diagnostic fingerprints for the same package set at the
# base and HEAD. HEAD compiler failures are fatal; existing sites may disappear,
# but new sites may not appear. If the base itself does not compile, retain every
# diagnostic it emitted before failing and compare HEAD against that partial
# baseline. Diagnostics remain enforceable only for packages that completed at
# least one base artifact; dependent packages the compiler never reached cannot
# provide evidence that a HEAD diagnostic is new.

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
base_metadata="$scratch/base-metadata.json"
head_metadata="$scratch/head-metadata.json"

cleanup() {
    git -C "$root" worktree remove --force "$base_tree" >/dev/null 2>&1 || true
    rm -rf "$scratch"
}
trap cleanup EXIT

git -C "$root" worktree add --quiet --detach "$base_tree" "$base" || exit 2
git -C "$root" diff --name-status -M "$base..HEAD" >"$changed_paths"

cargo metadata --manifest-path "$base_tree/Cargo.toml" --locked --offline \
    --no-deps --format-version 1 >"$base_metadata" || exit 2
cargo metadata --manifest-path "$root/Cargo.toml" --locked --offline \
    --no-deps --format-version 1 >"$head_metadata" || exit 2

head_args=("$@")
base_args=()
declare -A base_packages=()
while IFS= read -r package; do
    base_packages["$package"]=1
done < <(
    python3 -c 'import json,sys; print("\n".join(p["name"] for p in json.load(open(sys.argv[1]))["packages"]))' \
        "$base_metadata"
)
declare -A renamed_base_packages=()
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
        elif [ -n "${renamed_base_packages[$package]:-}" ]; then
            base_args+=("$argument" "${renamed_base_packages[$package]}")
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

python3 - "$base_json" "$head_json" "$base_tree" "$root" "$base_compiled" "$changed_paths" "$base_metadata" <<'PY'
import collections
import json
import pathlib
import sys


def diagnostics(path, tree, path_map=None):
    tree = pathlib.Path(tree).resolve()
    path_map = path_map or {}
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
                    try:
                        file_name = (tree / file_path).resolve().relative_to(tree).as_posix()
                    except ValueError:
                        continue
                if file_name.startswith(("vendor/", "../")):
                    continue
                file_name = path_map.get(file_name, file_name)
                # Count a diagnostic kind/message per file rather than treating
                # edited source text as a new lint site. Line/column remain in
                # `site` for de-duplication, while the Counter still detects an
                # increased number of equivalent warnings in that file.
                fingerprint = (code, file_name, message.get("message", ""), "")
                site = fingerprint + (span.get("line_start"), span.get("column_start"))
            if site in sites:
                continue
            sites.add(site)
            fingerprints[fingerprint] += 1
            examples[fingerprint].append(message.get("rendered") or message.get("message", ""))
    return fingerprints, examples


def completed_packages(path):
    packages = set()
    with open(path, encoding="utf-8") as stream:
        for line in stream:
            try:
                record = json.loads(line)
            except json.JSONDecodeError:
                continue
            if record.get("reason") == "compiler-artifact":
                package = record.get("package_id")
                if package:
                    packages.add(package)
    return packages


changed = set()
renames = {}
for line in pathlib.Path(sys.argv[6]).read_text(encoding="utf-8").splitlines():
    fields = line.split("\t")
    status = fields[0]
    if status.startswith("R") and len(fields) == 3:
        old_path, new_path = fields[1:]
        renames[old_path] = new_path
        changed.add(new_path)
    elif len(fields) >= 2:
        changed.add(fields[-1])

# Map base paths to their HEAD location so moving a file does not reset its lint
# budget. New duplicate sites still increase the counter and fail.
base, _ = diagnostics(sys.argv[1], sys.argv[3], renames)
head, examples = diagnostics(sys.argv[2], sys.argv[4])
introduced = head - base
if sys.argv[5] != "true":
    # An early compiler error can prevent Clippy from visiting entire dependent
    # packages. A missing diagnostic in one of those packages is not evidence
    # that HEAD introduced it. Keep enforcing the diff for changed files in
    # packages that produced a completed compiler artifact, and for new packages
    # that have no base baseline to preserve.
    metadata = json.load(open(sys.argv[7], encoding="utf-8"))
    compiled = completed_packages(sys.argv[1])
    package_roots = []
    for package in metadata["packages"]:
        manifest = pathlib.Path(package["manifest_path"]).resolve()
        relative_manifest = manifest.relative_to(pathlib.Path(sys.argv[3]).resolve()).as_posix()
        head_manifest = renames.get(relative_manifest, relative_manifest)
        package_roots.append((pathlib.PurePosixPath(head_manifest).parent, package["id"]))
    package_roots.sort(key=lambda entry: len(entry[0].parts), reverse=True)

    def base_package_for(file_name):
        path = pathlib.PurePosixPath(file_name)
        for root, package in package_roots:
            if path == root or root in path.parents:
                return package
        return None

    introduced = collections.Counter(
        {
            fingerprint: count
            for fingerprint, count in introduced.items()
            if fingerprint[1] == "<no-span>"
            or (
                fingerprint[1] in changed
                and (
                    base_package_for(fingerprint[1]) is None
                    or base_package_for(fingerprint[1]) in compiled
                )
            )
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
