#!/usr/bin/env bash
# Third-party code CI runs is pinned: every remote workflow action names an exact commit, the
# shared signing preflight binds the KMS key to keys/cosign.pub, and every external asset is an
# immutable, SHA-256-pinned download.
#
# Each problem is a finding on stdout, `path: message` about the file that has to change, and
# everything else goes to stderr.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fail=0
manifest="$repo/os/external-assets/github-assets.tsv"

if python3 "$repo/tools/policy/check_workflow_actions.py"; then
  printf 'ok    every remote workflow action is pinned to a full commit SHA\n' >&2
else
  fail=1
fi

if python3 "$repo/os/tests/test_require_cosign_authority.py" >/dev/null 2>&1; then
  printf 'ok    the shared signing preflight binds the KMS key to keys/cosign.pub\n' >&2
else
  printf 'os/dev/require-cosign-authority.sh: the shared signing preflight does not bind the KMS key to keys/cosign.pub\n'
  fail=1
fi

declare -A asset_names=()
while IFS=$'\t' read -r checksum name url extra; do
  case "$checksum" in ''|'#'*) continue ;; esac
  if [[ ! "$checksum" =~ ^[0-9a-f]{64}$ ]] \
    || [[ ! "$name" =~ ^[A-Za-z0-9][A-Za-z0-9._+-]*$ ]] \
    || [[ ! "$url" =~ ^https:// ]] \
    || [[ -n "$extra" ]] \
    || [[ -n "${asset_names[$name]:-}" ]]; then
    printf 'os/external-assets/github-assets.tsv: invalid or duplicate external-asset manifest row: %s\n' "$name"
    fail=1
    continue
  fi
  asset_names[$name]=1
  case "$url" in
    https://raw.githubusercontent.com/*)
      [[ "$url" =~ /[0-9a-f]{40}/ ]] || {
        printf 'os/external-assets/github-assets.tsv: mutable raw GitHub URL: %s\n' "$url"
        fail=1
      }
      ;;
    https://github.com/*/archive/*)
      [[ "$url" =~ /archive/[0-9a-f]{40}\.tar\.gz$ ]] || {
        printf 'os/external-assets/github-assets.tsv: mutable GitHub source archive URL: %s\n' "$url"
        fail=1
      }
      ;;
    https://github.com/*)
      [[ "$url" == */releases/download/* ]] && [[ "$url" != */releases/download/latest/* ]] || {
        printf 'os/external-assets/github-assets.tsv: unversioned GitHub release URL: %s\n' "$url"
        fail=1
      }
      ;;
  esac
done < "$manifest"
if [ "$fail" = 0 ]; then
  printf 'ok    external GitHub assets have safe names, immutable revisions, and SHA-256 pins\n' >&2
fi

exit "$fail"
