#!/usr/bin/env bash
# Publish one architecture's internal installer bundle to the Forgejo generic
# package registry on code.sandbox.example, then prove it is publicly served there.
#
#   CREDENTIAL_BROKER=/abs/broker GITHUB_REF_NAME=imager-vX.Y.Z \
#     apps/imager/packaging/publish-internal-bundle.sh BUNDLE_DIR
#
# The write:package token comes from the OIDC credential broker
# (acquire-release-token.sh internal-bundle), lives in RUNNER_TEMP only for
# this run, and is sent to code.sandbox.example only. FORGEJO_TOKEN may be given
# instead for a manual run.
#
# BUNDLE_DIR is os/dev/export-internal-bundle.sh's OUTDIR/bundle: exactly one
# manifest and its two component images. They land next to each other at
#
#   https://code.sandbox.example/api/packages/OWNER/generic/tilo-internal-installer/TAG/NAME
#
# because the Imager fetches each component by replacing the manifest URL's
# last segment. Why a generic package and not a release attachment: the two
# architectures publish independently from their own native lanes (no shared
# draft to race), the root image exceeds the default attachment limit, and
# the bytes never travel through workflow artifacts or reach the GitHub
# release. Why code.sandbox.example: the Imager accepts internal manifests from that
# origin only (trusted_catalog_url in apps/imager/src/release.rs), and follows
# no cross-origin redirect, so Forgejo must serve package blobs directly.
#
# Idempotent and immutable: an existing file is never replaced; it is
# accepted only when the public bytes equal the local ones. Components go
# first, the manifest last, so a published manifest always has its files.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
server="https://code.sandbox.example"
owner="${TILO_INTERNAL_BUNDLE_OWNER:-qos}"
package=tilo-internal-installer
bundle="${1:?usage: $0 BUNDLE_DIR}"
[[ "$GITHUB_REF_NAME" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
[[ "$owner" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$ ]] \
  || { printf 'package owner must be a Forgejo user or organisation name\n' >&2; exit 2; }
[ -d "$bundle" ] && [ ! -L "$bundle" ] || { printf 'not a bundle directory: %s\n' "$bundle" >&2; exit 2; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
mapfile -t manifests < <(find "$bundle" -maxdepth 1 -type f -name 'tilo-internal-installer-*.json' | sort)
[ "${#manifests[@]}" -eq 1 ] || { printf 'expected exactly one manifest in %s\n' "$bundle" >&2; exit 1; }
manifest="${manifests[0]}"
python3 "$repo/os/dev/internal-bundle.py" inspect --manifest "$manifest" >/dev/null \
  || { printf 'refusing to publish a bundle that does not inspect clean\n' >&2; exit 1; }
mapfile -t names < <(python3 - "$manifest" "$GITHUB_REF_NAME" <<'PY'
import json, sys
data = json.load(open(sys.argv[1], encoding='utf-8'))
if data['release_id'] != sys.argv[2]:
    raise SystemExit('manifest release_id does not match the release tag')
for component in data['components']:
    print(component['filename'])
PY
)
[ "${#names[@]}" -eq 2 ] || { printf 'manifest must name two components\n' >&2; exit 1; }
[ "$(find "$bundle" -mindepth 1 | wc -l | tr -d ' ')" = 3 ] \
  || { printf 'bundle directory holds files the manifest does not name\n' >&2; exit 1; }
names+=("$(basename "$manifest")")

work="$(mktemp -d /var/tmp/tilo-internal-publish.XXXXXX)"
token_file=""
trap 'find "$work" -depth -delete; [ -z "$token_file" ] || rm -f "$token_file"' EXIT
if [ -z "${FORGEJO_TOKEN:-}" ]; then
  [ -n "${CREDENTIAL_BROKER:-}" ] || {
    printf 'configure IMAGER_INTERNAL_BUNDLE_CREDENTIAL_BROKER for internal bundle publication\n' >&2
    exit 1
  }
  : "${RUNNER_TEMP:?RUNNER_TEMP is required to hold the brokered token}"
  token_file="$RUNNER_TEMP/tilo-internal-bundle-token"
  "$(dirname "${BASH_SOURCE[0]}")/acquire-release-token.sh" internal-bundle "$CREDENTIAL_BROKER" "$token_file"
  FORGEJO_TOKEN="$(cat "$token_file")"
fi
base="$server/api/packages/$owner/generic/$package/$GITHUB_REF_NAME"

public_sha256() {
  # Anonymous and without --location: the Imager downloads unauthenticated and
  # refuses cross-origin redirects, so anything but a direct 200 is a failure.
  local url="$1" status digest
  status="$(curl --silent --show-error --retry 5 --retry-all-errors --retry-max-time 300 \
    --connect-timeout 20 --output "$work/public" --write-out '%{http_code}' "$url")"
  [ "$status" = 200 ] || { printf 'public download of %s returned HTTP %s\n' "$url" "$status" >&2; return 1; }
  digest="$(sha256sum "$work/public" | cut -d' ' -f1)"
  rm -f "$work/public"
  printf '%s\n' "$digest"
}

for name in "${names[@]}"; do
  file="$bundle/$name"
  [ -f "$file" ] && [ ! -L "$file" ] || { printf 'missing regular bundle file %s\n' "$name" >&2; exit 1; }
  url="$base/$name"
  local_sha="$(sha256sum "$file" | cut -d' ' -f1)"
  status="$(curl --silent --show-error --connect-timeout 20 \
    -H "Authorization: token $FORGEJO_TOKEN" \
    --upload-file "$file" --output "$work/response" --write-out '%{http_code}' "$url")"
  case "$status" in
    201) printf 'uploaded %s\n' "$name" ;;
    409) printf 'already published %s; verifying its bytes\n' "$name" ;;
    *) printf 'upload of %s failed with HTTP %s: %s\n' "$name" "$status" \
         "$(head -c 400 "$work/response" 2>/dev/null)" >&2; exit 1 ;;
  esac
  remote_sha="$(public_sha256 "$url")"
  [ "$remote_sha" = "$local_sha" ] \
    || { printf 'published %s differs from the local bundle (%s != %s)\n' "$name" "$remote_sha" "$local_sha" >&2; exit 1; }
  printf 'verified public %s\n' "$url"
done
