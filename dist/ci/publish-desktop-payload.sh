#!/usr/bin/env bash
# Publish the OS desktop payloads a desktop package lane built (dist/ci-output/fedora-44-*/
# tilo-desktop-os-<arch>.tar.gz) as immutable generic packages, and write the `desktop-payload`
# pointer an OS build takes them from (t.toml: publish-desktop-payload).
#
#   dist/ci/publish-desktop-payload.sh POINTER
#
# Runs only in trusted runs of stable (T_REF, T_COMMIT), with REGISTRY_TOKEN: a token that may
# write generic packages. The pointer is JSON: {"schema": 1, "x86_64": {"url", "sha256"},
# "aarch64": {…} or null, "source": <commit>}.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
source tools/ci/publish/lib.sh

pointer=${1:?usage: publish-desktop-payload.sh POINTER}
[[ $(channel) == release ]] || die "the desktop payload is published from stable, not ${T_REF}"
require_commit
token=${REGISTRY_TOKEN:?the package token is not set: the check needs secrets = [\"REGISTRY_TOKEN\"]}
unset REGISTRY_TOKEN
base=https://code.tilo.so/api/packages/tilo/generic/tilo-desktop-os/0.0.0-$T_COMMIT

put_immutable() {
  local file=$1 url=$2 response status remote
  response=$(mktemp "$scratch/desktop-response.XXXXXX")
  status=$(printf 'header = "Authorization: token %s"\n' "$token" \
    | curl --silent --show-error --retry 3 --retry-all-errors --config - \
      --output "$response" --write-out '%{http_code}' --request PUT --upload-file "$file" "$url")
  case $status in
    200 | 201 | 204) ;;
    409)
      remote=$(mktemp "$scratch/desktop-existing.XXXXXX")
      curl --fail --silent --show-error --retry 3 --retry-all-errors "$url" --output "$remote"
      cmp --silent "$file" "$remote" || die "a different $url is already published"
      ;;
    *) cat "$response" >&2; die "uploading $url answered HTTP $status" ;;
  esac
}

shopt -s nullglob
entries=()
for bundle in dist/ci-output/fedora-44-*/tilo-desktop-os-*.tar.gz; do
  arch=${bundle##*tilo-desktop-os-}
  arch=${arch%.tar.gz}
  (cd "$(dirname "$bundle")" && sha256sum --check --strict "${bundle##*/}.sha256")
  url=$base/${bundle##*/}
  put_immutable "$bundle" "$url"
  put_immutable "$bundle.sha256" "$url.sha256"
  entries+=(--arg "url_$arch" "$url" --arg "sha256_$arch" "$(awk '{print $1}' "$bundle.sha256")")
done
(( ${#entries[@]} )) || die 'no desktop payload in dist/ci-output'
mkdir -p "$(dirname "$pointer")"
jq -n "${entries[@]}" --arg source "$T_COMMIT" '{schema: 1,
  x86_64: (if $ARGS.named.url_x86_64 then {url: $ARGS.named.url_x86_64, sha256: $ARGS.named.sha256_x86_64} else null end),
  aarch64: (if $ARGS.named.url_aarch64 then {url: $ARGS.named.url_aarch64, sha256: $ARGS.named.sha256_aarch64} else null end),
  source: $source}' > "$pointer"
jq -c . "$pointer"
