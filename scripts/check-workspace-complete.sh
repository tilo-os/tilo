#!/usr/bin/env bash
# Validate workspace membership without invoking Cargo.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
exec python3 tools/policy/check_workspace_members.py --root .
