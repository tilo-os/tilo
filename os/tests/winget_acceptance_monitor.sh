#!/usr/bin/env bash
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
cat >"$tmp/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = clone ] || { printf 'unexpected git operation\n' >&2; exit 1; }
destination="${!#}"
mkdir -p "$destination/src/data"
cp "$TILO_TEST_SITE_MANIFEST" "$destination/src/data/imager-release.json"
SH
chmod +x "$tmp/bin/git"

run_monitor() {
  TILO_TEST_SITE_MANIFEST="$1" \
  GITHUB_SERVER_URL=https://forgejo.invalid \
  TILO_SITE_GIT_TOKEN=test-token \
  PATH="$tmp/bin:$PATH" \
    "$repo/apps/imager/packaging/monitor-winget-acceptance.sh"
}

cat >"$tmp/unpublished.json" <<'JSON'
{"published":false,"tag":null,"channels":{"winget":"unavailable"}}
JSON
run_monitor "$tmp/unpublished.json" \
  | grep -Fq 'has no published Tilo Imager release'

cat >"$tmp/available.json" <<'JSON'
{"published":true,"tag":"imager-v1.2.3","channels":{"winget":"available"}}
JSON
run_monitor "$tmp/available.json" \
  | grep -Fq 'already available for imager-v1.2.3'

cat >"$tmp/bad-tag.json" <<'JSON'
{"published":true,"tag":"main","channels":{"winget":"submitted"}}
JSON
if run_monitor "$tmp/bad-tag.json" >"$tmp/out" 2>"$tmp/err"; then
  printf 'monitor accepted a non-release tag\n' >&2
  exit 1
fi
grep -Fq 'invalid tag: main' "$tmp/err"

cat >"$tmp/bad-state.json" <<'JSON'
{"published":true,"tag":"imager-v1.2.3","channels":{"winget":"unknown"}}
JSON
if run_monitor "$tmp/bad-state.json" >"$tmp/out" 2>"$tmp/err"; then
  printf 'monitor accepted an unknown WinGet state\n' >&2
  exit 1
fi
grep -Fq 'invalid WinGet state: unknown' "$tmp/err"

printf 'ok    WinGet acceptance monitor is fail-closed and version-bound\n'
