#!/usr/bin/env bash
# Check first-party manifests, including standalone tools, without rewriting
# imported dependencies to satisfy our heuristic unused-dependency scanner.
#
#   check-unused-dependencies.sh                          # every first-party crate
#   check-unused-dependencies.sh --changed BASE BEFORE    # crates a CI change touches
#
# A dependency can only become unused through an edit inside its crate (its
# manifest or its sources) or through the workspace manifest that crates
# inherit from. `--changed` therefore scans the crates that own a changed or
# deleted path. Pure literal workspace-member additions scan those new crates;
# inherited settings, structural changes, or checker changes scan every crate. BASE/BEFORE have the same pull-request/push meaning as the
# formatting and allow-reason gates.
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=${TILO_REPO_ROOT:-$(cd "$script_dir/../.." && pwd)}
cd "$repo"
paths=$(mktemp)
trap 'rm -f -- "$paths"' EXIT
git ls-files -z --cached --others --exclude-standard -- \
    Cargo.toml '*/Cargo.toml' ':(top,exclude)vendor/**' >"$paths"
mapfile -d '' manifests <"$paths"
(( ${#manifests[@]} > 0 )) || { echo 'no first-party Cargo manifests found' >&2; exit 1; }

# The crate `cargo hakari` generates names dependencies only to unify their
# features, so every one of them is unused by design; never scan it.
declare -A generated=()
for manifest in "${manifests[@]}"; do
    if [[ -f $manifest ]] && grep -qx '### BEGIN HAKARI SECTION' "$manifest"; then
        generated[${manifest%/Cargo.toml}]=1
    fi
done
holds_generated() {
    local dir
    for dir in "${!generated[@]}"; do
        [[ $dir == "$1"/* ]] && return 0
    done
    return 1
}

scan=()
if [[ ${1:-} == --changed ]]; then
    range=$(python3 "$script_dir/check_changed_rust_allows.py" \
        --root "$repo" --comparison-range "${2:-}" "${3:-}")
    declare -A crate_dirs=()
    for manifest in "${manifests[@]}"; do
        [[ $manifest == Cargo.toml ]] || crate_dirs[${manifest%/Cargo.toml}]=1
    done
    # List both sides of renames: the crate a file left may lose its last use.
    git diff --name-only -z --no-renames "$range" >"$paths"
    mapfile -d '' changed <"$paths"
    declare -A selected=()
    full=0
    for path in "${changed[@]}"; do
        case $path in
            tools/ci/check-unused-dependencies.sh|tools/ci/workspace_member_additions.py)
                full=1; break ;;
            Cargo.toml)
                if python3 "$script_dir/workspace_member_additions.py" --root "$repo" "$range" >"$paths"; then
                    mapfile -d '' added_members <"$paths"
                    for member in "${added_members[@]}"; do
                        [[ -n ${generated[$member]:-} ]] || selected[$member]=1
                    done
                else
                    full=1; break
                fi
                continue ;;
        esac
        dir=$path
        while [[ $dir == */* ]]; do
            dir=${dir%/*}
            if [[ -n ${crate_dirs[$dir]:-} ]]; then
                [[ -n ${generated[$dir]:-} ]] || selected[$dir]=1
                break
            fi
        done
    done
    if (( ! full )); then
        if (( ${#selected[@]} == 0 )); then
            echo "unused dependencies OK (no first-party crate changed in $range)"
            exit 0
        fi
        printf '%s\0' "${!selected[@]}" | sort -z >"$paths"
        mapfile -d '' scan <"$paths"
    fi
fi

if (( ${#scan[@]} == 0 )); then
    # Scan each first-party subtree once so machete can analyze its crates in parallel.
    # Passing each manifest separately would serialize cargo metadata across the fleet.
    # A subtree holding the generated crate is scanned as its other subtrees.
    declare -A seen=()
    for manifest in "${manifests[@]}"; do
        dir=${manifest%/Cargo.toml}
        [[ -z ${generated[$dir]:-} ]] || continue
        root=${manifest%%/*}
        while [[ $root != "$dir" ]] && holds_generated "$root"; do
            rest=${manifest#"$root"/}
            root=$root/${rest%%/*}
        done
        if [[ -z ${seen[$root]:-} ]]; then
            scan+=("$root")
            seen[$root]=1
        fi
    done
fi
cargo machete --with-metadata "${scan[@]}"
