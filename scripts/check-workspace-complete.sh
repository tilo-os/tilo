#!/usr/bin/env bash
# Fail if any crate on disk (apps/*, crates/**) is missing from the workspace
# members. Fast, no cargo.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
fail=0
while IFS= read -r ct; do
  dir="${ct%/Cargo.toml}"
  # Skip crates that declare their own [workspace] (nested / test fixtures).
  grep -qE '^\[workspace\]' "$ct" && continue
  grep -qF "\"$dir\"" Cargo.toml || { echo "  not a workspace member: $dir"; fail=1; }
done < <(find apps crates -mindepth 2 -name Cargo.toml 2>/dev/null | sort)
if [ "$fail" -ne 0 ]; then
  echo "FAIL: add the crate(s) above to [workspace].members in Cargo.toml" >&2
  exit 1
fi
echo "workspace membership OK ($(grep -cE '"(apps|crates)/' Cargo.toml) member crates)"
