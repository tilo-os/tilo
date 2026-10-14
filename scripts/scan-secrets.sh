#!/usr/bin/env bash
# Look for credentials in the staged change. Reads added lines only, so an
# existing string does not re-fire on every commit that touches its file.
#
#   scan-secrets.sh            scan the staged change
#   scan-secrets.sh --all      scan every tracked file (an audit)
#
# The credential shapes and the same-line exception live in one place,
# tools/policy/added_line_policy.py, which the pre-commit hook and CI's
# per-commit check (tools/policy/check_commit_policy.py) also use.
set -euo pipefail
root=$(git rev-parse --show-toplevel)
policy="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/tools/policy/added_line_policy.py"
exec python3 "$policy" --root "$root" --secrets "$@"
