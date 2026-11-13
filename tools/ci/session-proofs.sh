#!/usr/bin/env bash
# Prove the session end to end: the proofs in scripts/headless-examples that
# drive a headless session of their own and read every claim back off it.
#
# Each proof starts a session on a scratch home and a private runtime
# directory, so nothing here touches the desktop on this machine. They are
# pointed at the binaries Cargo built for this run, and never at anything
# installed.
#
# Usage: tools/ci/session-proofs.sh
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)
cd "$repo_root"

target=$(realpath -m "${CARGO_TARGET_DIR:-target}")
# The binaries may already be there from the workspace suite; building them
# keeps this step runnable on its own.
cargo build --locked -p tilo-session -p tilo-control

export TILO_SHELL_BINARY="$target/debug/tilo-session"
export TILO_CONTROL_BINARY="$target/debug/tilo-control"
# The proofs are scripts, not modules: nothing may drop bytecode into the tree.
export PYTHONDONTWRITEBYTECODE=1

proofs=(
  agent-seats-proof.py
  session-lock-proof.py
)

status=0
for proof in "${proofs[@]}"; do
  echo "=== $proof"
  python3 "scripts/headless-examples/$proof" || status=1
done
exit "$status"
