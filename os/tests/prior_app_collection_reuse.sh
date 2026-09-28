#!/usr/bin/env bash

set -euo pipefail

repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/bin" "$work/tmp" "$work/release/runtime/tilo-runtime/root" \
  "$work/prior/tapp" "$work/prior/runtime/tilo-runtime/root"

root=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
printf 'root_hash = "%s"\n' "$root" > "$work/release/runtime/tilo-runtime/root/bundle.toml"
printf 'root_hash = "%s"\n' "$root" > "$work/prior/runtime/tilo-runtime/root/bundle.toml"
printf 'old package\n' > "$work/prior/tapp/old-1.0.1-dev.9+ga-x86_64.tapp"
printf '{"apps":[{"name":"old","id":"so.tilo.old","package":"old","version":"1.0.1-dev.9+ga","declared_version":"1.0.0","runtime":"tilo-runtime/1.0.0"}]}\n' > "$work/prior/index.json"
printf 'checksums\n' > "$work/prior/SHA256SUMS"
tar -C "$work/prior" -czf "$work/prior.tar.gz" tapp runtime index.json SHA256SUMS
prior_sha=$(sha256sum "$work/prior.tar.gz" | cut -d' ' -f1)

cat > "$work/bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=
header=
url=
while (($#)); do
  case "$1" in
    --output) output=$2; shift 2 ;;
    --header) header=$2; shift 2 ;;
    --retry|--proto|--proto-redir) shift 2 ;;
    --fail|--silent|--show-error|--retry-all-errors) shift ;;
    *) url=$1; shift ;;
  esac
done
printf '%s\t%s\n' "$header" "$url" >> "$CURL_LOG"
[[ $header == 'Authorization: Bearer reuse-token' ]]
[[ $url == https://code.tilo.so/api/packages/tilo/generic/tilo-app-collections/v1/tilo-apps-x86_64.tar.gz ]]
cp "$PRIOR_FIXTURE" "$output"
EOF
chmod +x "$work/bin/curl"

export PATH="$work/bin:$PATH"
export RUNNER_TEMP="$work/tmp"
export TILO_PACKAGE_TOKEN=reuse-token
export TILO_PACKAGE_PUBLIC_BASE_URL=https://code.tilo.so/api/packages/tilo/generic
export PRIOR_FIXTURE="$work/prior.tar.gz"
export CURL_LOG="$work/curl.log"

python3 - "$work" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
apps = [dict(name=name, id='so.tilo.' + name, package=name, version='1.0.0', runtime='tilo-runtime/1.0.0') for name in ('old', 'new')]
(root / 'release/index.json').write_text(json.dumps({'apps': apps}))
(root / 'plan.json').write_text(json.dumps({'catalog': apps}))
PY
PATH="$work/bin:$PATH" tools/reuse_prior_app_collection.sh \
  "$work/release/index.json" "$work/plan.json" "$work/full-out" x86_64 '' ''
[[ ! -e $CURL_LOG ]]

printf '{"apps":[{"name":"new","id":"so.tilo.new","package":"new","version":"1.0.1-dev.10+gb","declared_version":"1.0.0","runtime":"tilo-runtime/1.0.0"}]}\n' > "$work/release/index.json"
internal_url=http://forgejo.forgejo.svc.cluster.local:3000/api/packages/tilo/generic/tilo-app-collections/v1/tilo-apps-x86_64.tar.gz
tools/reuse_prior_app_collection.sh "$work/release/index.json" "$work/plan.json" \
  "$work/partial-out" x86_64 "$internal_url" "$prior_sha"
cmp "$work/prior/tapp/old-1.0.1-dev.9+ga-x86_64.tapp" "$work/partial-out/old-1.0.1-dev.9+ga-x86_64.tapp"
jq -e '.catalog[] | select(.name == "old") | .version == "1.0.1-dev.9+ga"' "$work/plan.json" >/dev/null
jq -e '.catalog[] | select(.name == "new") | .version == "1.0.1-dev.10+gb"' "$work/plan.json" >/dev/null

grep -Fxq $'Authorization: Bearer reuse-token\thttps://code.tilo.so/api/packages/tilo/generic/tilo-app-collections/v1/tilo-apps-x86_64.tar.gz' "$CURL_LOG"

# Flip the first hex digit so the wrong digest can never equal the real one
# (a fixed "00" prefix matched whenever the archive's digest began with 00).
if [[ ${prior_sha:0:1} == 0 ]]; then wrong_sha=1${prior_sha:1}; else wrong_sha=0${prior_sha:1}; fi
if tools/reuse_prior_app_collection.sh "$work/release/index.json" "$work/plan.json" \
    "$work/bad-digest-out" x86_64 "$internal_url" "$wrong_sha" >/dev/null 2>&1; then
  printf 'digest mismatch was accepted\n' >&2
  exit 1
fi

mkdir -p "$work/bad/tapp" "$work/bad/runtime/tilo-runtime/root"
cp "$work/prior/tapp/old-1.0.1-dev.9+ga-x86_64.tapp" "$work/bad/tapp/"
cp "$work/prior/runtime/tilo-runtime/root/bundle.toml" "$work/bad/runtime/tilo-runtime/root/"
printf 'unexpected\n' > "$work/bad/escape"
tar -C "$work/bad" -czf "$work/bad.tar.gz" tapp runtime escape
export PRIOR_FIXTURE="$work/bad.tar.gz"
bad_sha=$(sha256sum "$work/bad.tar.gz" | cut -d' ' -f1)
if tools/reuse_prior_app_collection.sh "$work/release/index.json" "$work/plan.json" \
    "$work/bad-content-out" x86_64 "$internal_url" "$bad_sha" >/dev/null 2>&1; then
  printf 'unexpected archive content was accepted\n' >&2
  exit 1
fi

printf 'prior app collection reuse tests passed\n'
