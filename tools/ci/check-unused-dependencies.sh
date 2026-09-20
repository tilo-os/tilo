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
cargo machete --with-metadata "${manifests[@]}"
