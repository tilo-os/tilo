#!/usr/bin/env bash
# Format-check exactly the Rust files changed from a CI comparison base.
set -euo pipefail

repo="${TILO_REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
cd "$repo"

base="${1:-}"
before="${2:-}"
if [ -z "$base" ] || [ "$base" = null ]; then
    base="$before"
fi
if [ -z "$base" ] || [ "$base" = 0000000000000000000000000000000000000000 ]; then
    base=$(git rev-parse HEAD^)
fi
if ! git cat-file -e "$base^{commit}" 2>/dev/null; then
    git fetch --no-tags --depth=1 origin "$base"
fi

if merge_base=$(git merge-base "$base" HEAD 2>/dev/null); then
    range="$merge_base...HEAD"
else
    range="$base..HEAD"
fi

mapfile -d '' rust_files < <(
    git diff --name-only -z --diff-filter=ACMRTUXB "$range" -- '*.rs'
)
if [ "${#rust_files[@]}" -eq 0 ]; then
    echo "changed Rust formatting OK (no Rust files in $range)"
    exit 0
fi

rustfmt --edition 2024 --check -- "${rust_files[@]}"
echo "changed Rust formatting OK (${#rust_files[@]} files)"
