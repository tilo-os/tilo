#!/usr/bin/env bash
# Format-check exactly the Rust files changed from a CI comparison base.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo="${TILO_REPO_ROOT:-$(cd "$script_dir/../.." && pwd)}"
cd "$repo"

# Use the same PR-versus-push semantics and shallow-history recovery as the
# allow-reason gate; an unavailable comparison must fail both checks.
range=$(python3 "$script_dir/check_changed_rust_allows.py" \
    --root "$repo" --comparison-range "${1:-}" "${2:-}")

# Imported dependencies and checked-in Cap'n Proto bindings retain generator
# formatting; their native tests and dependency/security checks still run.
# Process substitution hides git diff's exit status from mapfile/set -e.
changed_paths=$(mktemp)
trap 'rm -f -- "$changed_paths"' EXIT
git diff --name-only -z --diff-filter=ACMRTUXB "$range" -- \
    '*.rs' \
    ':(top,exclude)vendor/**' \
    ':(top,exclude)**/src/generated/*_capnp.rs' >"$changed_paths"
mapfile -d '' rust_files <"$changed_paths"
if [ "${#rust_files[@]}" -eq 0 ]; then
    echo "changed Rust formatting OK (no Rust files in $range)"
    exit 0
fi

rustfmt --edition 2024 --check -- "${rust_files[@]}"
echo "changed Rust formatting OK (${#rust_files[@]} files)"
