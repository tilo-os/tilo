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

# A submitted release is advanced with the download root and signing key it was
# published with, never with values from the environment.
mkdir -p "$tmp/work/apps/imager/packaging"
cat >"$tmp/work/apps/imager/packaging/publish-site-release.sh" <<'SH'
#!/usr/bin/env bash
printf '%s|%s|%s|%s\n' "$GITHUB_REF_NAME" "$TILO_RELEASE_SERVER" \
  "$TILO_RELEASE_REPOSITORY" "$TILO_LINUX_SIGNING_FINGERPRINT"
SH
chmod +x "$tmp/work/apps/imager/packaging/publish-site-release.sh"
fingerprint=0123456789ABCDEF0123456789ABCDEF01234567
cat >"$tmp/submitted.json" <<JSON
{"published":true,"tag":"imager-v1.2.3","channels":{"winget":"submitted"},
 "releaseRoot":"https://github.com/tilo-os/releases/releases","linuxSigningFingerprint":"$fingerprint"}
JSON
(cd "$tmp/work" && TILO_RELEASE_REPOSITORY=attacker/repo TILO_LINUX_SIGNING_FINGERPRINT=other \
  run_monitor "$tmp/submitted.json") >"$tmp/out"
grep -Fxq "imager-v1.2.3|https://github.com|tilo-os/releases|$fingerprint" "$tmp/out"

for broken in '"releaseRoot":"https://github.com/tilo-os/releases"' \
              '"releaseRoot":"http://github.com/tilo-os/releases/releases"' \
              '"linuxSigningFingerprint":""'; do
  jq -c ". + {$broken}" "$tmp/submitted.json" >"$tmp/broken.json"
  if (cd "$tmp/work" && run_monitor "$tmp/broken.json") >"$tmp/out" 2>"$tmp/err"; then
    printf 'monitor advanced a release whose manifest has %s\n' "$broken" >&2
    exit 1
  fi
  if grep -Fq 'imager-v1.2.3|' "$tmp/out"; then
    printf 'monitor published a release whose manifest has %s\n' "$broken" >&2
    exit 1
  fi
done

printf 'ok    WinGet acceptance monitor is fail-closed and version-bound\n'
