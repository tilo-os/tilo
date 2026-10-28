#!/usr/bin/env bash
# Run the own test suites of vendored crates that carry Tilo patches.
#
#   tools/ci/test-vendored-crates.sh [vendor/CRATE ...]
#
# These crates are path dependencies outside the workspace, so the workspace's
# `cargo test` builds them but never runs their tests. With no argument, runs
# every crate in VENDORED_TEST_CRATES (tools/ci/package_sets.py); t.toml's
# `vendored` check runs them when their files change.
#
# Each crate is its own workspace. Its lock file (ignored by git) starts as the
# workspace's Cargo.lock, so its dependencies are tested at the versions Tilo
# ships; Cargo drops the entries it does not use and resolves only the crate's
# own dev-dependencies afresh.
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"

if [ "$#" -eq 0 ]; then
  read -r -a crates < <(python3 -c '
import sys
sys.path.insert(0, "tools")
from ci.package_sets import VENDORED_TEST_CRATES
print(" ".join(VENDORED_TEST_CRATES))
')
  set -- "${crates[@]}"
fi

for crate in "$@"; do
  case "$crate" in
    vendor/*) ;;
    *) echo "not a vendored crate: $crate" >&2; exit 2 ;;
  esac
  # A crate without its own [workspace] would resolve into Tilo's workspace,
  # or fail to, and run nothing of its own.
  if ! grep -qx '\[workspace\]' "$crate/Cargo.toml"; then
    echo "$crate/Cargo.toml has no [workspace] table" >&2
    exit 1
  fi
  dir=$crate
  if [ ! -w "$crate" ]; then
    # A read-only checkout (a t check) takes the lock in a copy of the
    # vendored tree, beside the workspace's Cargo configuration and the test
    # runner that configuration names.
    copy=$(mktemp -d)
    trap 'rm -rf -- "$copy"' EXIT
    cp -a vendor tools .cargo rust-toolchain.toml "$copy/"
    dir=$copy/$crate
  fi
  cp Cargo.lock "$dir/Cargo.lock"
  echo "== cargo test in $crate"
  (cd "$dir" && cargo test)
done
