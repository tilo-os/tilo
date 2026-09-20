#!/usr/bin/env bash
# Check first-party manifests, including standalone tools, without rewriting
# imported dependencies to satisfy our heuristic unused-dependency scanner.
set -euo pipefail
repo=${TILO_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}
cd "$repo"
paths=$(mktemp)
trap 'rm -f -- "$paths"' EXIT
git ls-files -z --cached --others --exclude-standard -- \
    Cargo.toml '*/Cargo.toml' ':(top,exclude)vendor/**' >"$paths"
mapfile -d '' manifests <"$paths"
(( ${#manifests[@]} > 0 )) || { echo 'no first-party Cargo manifests found' >&2; exit 1; }
# Scan each first-party subtree once so machete can analyze its crates in parallel.
# Passing each manifest separately would serialize cargo metadata across the fleet.
declare -A seen=()
roots=()
for manifest in "${manifests[@]}"; do
    root=${manifest%%/*}
    if [[ -z ${seen[$root]:-} ]]; then
        roots+=("$root")
        seen[$root]=1
    fi
done
cargo machete --with-metadata "${roots[@]}"
