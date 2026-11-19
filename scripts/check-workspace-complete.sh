#!/usr/bin/env bash
# Validate workspace membership without invoking Cargo.
# Arguments pass through: the pre-commit hook adds --staged to judge only the
# crate manifests the commit changes, as the policy suite and CI judge them all.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
exec python3 tools/policy/check_workspace_members.py --root . "$@"
