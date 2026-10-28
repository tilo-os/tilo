#!/usr/bin/env bash
# Publish one architecture's signed target policy bundle to the Forgejo generic
# package registry on code.tilo.so, then prove it is publicly served there.
#
#   CREDENTIAL_BROKER=/abs/broker GITHUB_REF_NAME=imager-vX.Y.Z \
#     apps/imager/packaging/publish-target-policy.sh BUNDLE_DIR
#
# BUNDLE_DIR is export-target-policy.py's output directory: one
# `tilo-target-policy-<ARCH>.tar` and the `target-policy-catalog-<ARCH>.json`
# fragment that names it. They land at
#
#   https://code.tilo.so/api/packages/OWNER/generic/tilo-target-policy/TAG/NAME
#
# The write:package token comes from the existing internal-bundle credential
# broker exchange, lives in RUNNER_TEMP only for this run, and is sent to
# code.tilo.so only. FORGEJO_TOKEN may be given instead for a manual run.
#
# Idempotent and immutable: an existing file is never replaced; it is accepted
# only when the public bytes equal the local ones. The archive goes first and
# the fragment last, so a published fragment always has its archive.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
server="https://code.tilo.so"
owner="${TILO_TARGET_POLICY_OWNER:-tilo}"
package=tilo-target-policy
bundle="${1:?usage: $0 BUNDLE_DIR}"
[[ "$GITHUB_REF_NAME" =~ ^imager-v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
[[ "$owner" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$ ]] \
  || { printf 'package owner must be a Forgejo user or organisation name\n' >&2; exit 2; }
[ -d "$bundle" ] && [ ! -L "$bundle" ] || { printf 'not a bundle directory: %s\n' "$bundle" >&2; exit 2; }

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
python3 "$repo/apps/imager/packaging/export-target-policy.py" verify \
  --bundle "$bundle" --release-id "$GITHUB_REF_NAME" --owner "$owner" >/dev/null \
  || { printf 'refusing to publish a target policy bundle that does not verify\n' >&2; exit 1; }
mapfile -t fragments < <(find "$bundle" -maxdepth 1 -type f -name 'target-policy-catalog-*.json' -print | sort)
[ "${#fragments[@]}" -eq 1 ] || { printf 'expected exactly one catalog fragment in %s\n' "$bundle" >&2; exit 1; }
fragment="${fragments[0]}"
mapfile -t archives < <(find "$bundle" -maxdepth 1 -type f -name 'tilo-target-policy-*.tar' -print | sort)
[ "${#archives[@]}" -eq 1 ] || { printf 'expected exactly one target policy archive in %s\n' "$bundle" >&2; exit 1; }
[ "$(find "$bundle" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" = 2 ] \
  || { printf 'bundle directory holds files the fragment does not name\n' >&2; exit 1; }

work="$(mktemp -d "${TMPDIR:-/var/tmp}/tilo-target-policy-publish.XXXXXX")"
token_file=""
trap 'find "$work" -depth -delete; [ -z "$token_file" ] || rm -f "$token_file"' EXIT
if [ -z "${FORGEJO_TOKEN:-}" ]; then
  [ -n "${CREDENTIAL_BROKER:-}" ] || {
    printf 'configure IMAGER_INTERNAL_BUNDLE_CREDENTIAL_BROKER for target policy publication\n' >&2
    exit 1
  }
  : "${RUNNER_TEMP:?RUNNER_TEMP is required to hold the brokered token}"
  token_file="$RUNNER_TEMP/tilo-target-policy-token"
  "$(dirname "${BASH_SOURCE[0]}")/acquire-release-token.sh" internal-bundle "$CREDENTIAL_BROKER" "$token_file"
  FORGEJO_TOKEN="$(cat "$token_file")"
fi
base="$server/api/packages/$owner/generic/$package/$GITHUB_REF_NAME"

public_bytes() {
  # Anonymous and without --location: the Imager downloads unauthenticated and
  # refuses cross-origin redirects, so anything but a direct 200 is a failure.
  local url="$1" status
  status="$(curl --silent --show-error --retry 5 --retry-all-errors --retry-max-time 300 \
    --connect-timeout 20 --output "$work/public" --write-out '%{http_code}' "$url")"
  [ "$status" = 200 ] || { printf 'public download of %s returned HTTP %s\n' "$url" "$status" >&2; return 1; }
}

for file in "${archives[0]}" "$fragment"; do
  name="$(basename "$file")"
  [ -f "$file" ] && [ ! -L "$file" ] || { printf 'missing regular bundle file %s\n' "$name" >&2; exit 1; }
  url="$base/$name"
  local_sha="$(sha256sum "$file" | cut -d' ' -f1)"
  local_size="$(wc -c <"$file" | tr -d '[:space:]')"
  status="$(curl --silent --show-error --connect-timeout 20 \
    -H "Authorization: token $FORGEJO_TOKEN" \
    --upload-file "$file" --output "$work/response" --write-out '%{http_code}' "$url")"
  case "$status" in
    201) printf 'uploaded %s\n' "$name" ;;
    409) printf 'already published %s; verifying its bytes\n' "$name" ;;
    *) printf 'upload of %s failed with HTTP %s: %s\n' "$name" "$status" \
         "$(head -c 400 "$work/response" 2>/dev/null)" >&2; exit 1 ;;
  esac
  public_bytes "$url"
  remote_sha="$(sha256sum "$work/public" | cut -d' ' -f1)"
  remote_size="$(wc -c <"$work/public" | tr -d '[:space:]')"
  rm -f "$work/public"
  [ "$remote_sha" = "$local_sha" ] && [ "$remote_size" = "$local_size" ] \
    || { printf 'published %s differs from the local bundle\n' "$name" >&2; exit 1; }
  printf 'verified public %s\n' "$url"
done
