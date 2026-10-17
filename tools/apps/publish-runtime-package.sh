#!/usr/bin/env bash
# Offer a runtime package (pack-runtime-package.sh output) in the registry.
#
#   publish-runtime-package.sh <package-dir> <arch> <package-id> <artifact-name> <metadata.json>
#
# Uploads the thin package and its content-addressed payload objects side by
# side as the immutable generic package <artifact-name>/$VERSION, then submits
# the publisher-signed registry entry naming the runtime it provides. A runtime
# the registry already offers for <arch> is left alone. Environment: VERSION,
# TILO_PACKAGE_TOKEN, PACKAGE_API (the authenticated generic-package base),
# REGISTRY_URL, OWNER, REGISTRY_CLI (tilo-app-package), REGISTRY_IDENTITY
# (publisher identity file), RUNNER_TEMP.
#
# RUNTIME_PACKAGE_STAGE splits the two by credential: `upload` only uploads,
# with TILO_PACKAGE_TOKEN and no identity or metadata; `submit` only signs and
# submits the entry, with REGISTRY_IDENTITY and no package token, after an
# upload stage put the objects in place. Unset, it does both.
set -euo pipefail
stage=${RUNTIME_PACKAGE_STAGE:-all}
dir=$1
arch=$2
package_id=$3
artifact=$4
metadata=${5:-}
: "${VERSION:?}" "${PACKAGE_API:?}" "${REGISTRY_URL:?}" "${OWNER:?}" "${REGISTRY_CLI:?}"
case $stage in
  upload) : "${TILO_PACKAGE_TOKEN:?}" ;;
  submit) : "${REGISTRY_IDENTITY:?}" "${metadata:?}" ;;
  all) : "${TILO_PACKAGE_TOKEN:?}" "${REGISTRY_IDENTITY:?}" "${metadata:?}" ;;
  *) printf 'RUNTIME_PACKAGE_STAGE must be upload, submit or all\n' >&2; exit 2 ;;
esac
temp=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

tapp=$(find "$dir" -maxdepth 1 -name '*.tapp' -print -quit)
[[ -n $tapp ]] || { printf 'no runtime package in %s\n' "$dir" >&2; exit 1; }
provides=$("$REGISTRY_CLI" inspect "$tapp" \
  | sed -n 's/^provides *= *"\(tilo-runtime\/[^"]*\)".*/\1/p' | head -1)
service_version=
if [[ $package_id == so.tilo.signal-provider ]]; then
  # This explicitly selected executable consumes a runtime. Registry admission
  # still owns publisher and package authority, exactly as for runtime packages.
  inspection=$("$REGISTRY_CLI" inspect "$tapp")
  provides=$(printf '%s\n' "$inspection" | sed -n 's/^runtime *= *"\(tilo-runtime\/[^" ]*\)".*/\1/p' | head -1)
  service_version=$(printf '%s\n' "$inspection" | sed -n 's/^version *= *"\([^" ]*\)".*/\1/p' | head -1)
  [[ $provides =~ ^tilo-runtime/2026\.10\.1-signal\+native\.[0-9a-f]{32}$ && -n $service_version ]] || {
    printf 'Signal provider runtime must name its exact native build\n' >&2
    exit 1
  }
fi
[[ -n $provides ]] || { printf '%s names no runtime it provides\n' "$tapp" >&2; exit 1; }

snapshot="$temp/tilo-runtime-snapshot.json"
response="$temp/tilo-runtime-response.txt"
fetch_snapshot() {
  curl --fail --silent --show-error --connect-timeout 10 --max-time 30 \
    --retry 3 --retry-all-errors "$REGISTRY_URL/entries" --output "$snapshot"
  jq -e '.entries | type == "array"' "$snapshot" >/dev/null
}
fetch_snapshot
if jq -e --arg pkg "$package_id" --arg id "$provides" --arg arch "$arch" --arg version "$service_version" \
    '[.entries[] | select(.package_id == $pkg) | .versions[]
      | select(.target.runtime == $id and .target.architecture == $arch)
      | select($version == "" or .version == $version)] | length > 0' \
    "$snapshot" >/dev/null; then
  echo "::notice::the Tilo registry already offers $provides for $arch"
  exit 0
fi

if [[ $stage != submit ]]; then
uploads="$temp/tilo-runtime-uploads-$arch.tsv"
: > "$uploads"
while IFS= read -r object; do
  printf '%s\t%s\n' "$object" "$PACKAGE_API/$artifact/$VERSION/${object##*/}" >> "$uploads"
done < <(find "$dir" -maxdepth 1 -type f | sort)
# The thin package's compressed twin, which clients fetch first. It stays out
# of $dir, whose files `entry --objects` reads as the payload objects. The
# registry writes the payload objects' twins itself when it stores them.
twin="$temp/tilo-runtime-twin-$arch/${tapp##*/}.zst"
mkdir -p "${twin%/*}"
"$REGISTRY_CLI" compress -o "$twin" "$tapp"
printf '%s\t%s\n' "$twin" "$PACKAGE_API/$artifact/$VERSION/${twin##*/}" >> "$uploads"
TILO_PACKAGE_UPLOAD_WORKERS=8 "$here/../upload_immutable_packages.sh" "$uploads"
fi
[[ $stage != upload ]] || exit 0

for attempt in 1 2 3; do
  (( attempt == 1 )) || fetch_snapshot
  previous="$temp/tilo-runtime-previous.json"
  request="$temp/tilo-runtime-request.json"
  rm -f "$previous" "$request"
  # A request still waiting for review is replaced by this one and must keep
  # its versions (the other architecture's, say), so build on it.
  "$REGISTRY_CLI" pending --registry "$REGISTRY_URL" \
    --identity "$REGISTRY_IDENTITY" "$package_id" > "$previous"
  # Whether the package is published decides add or update; a request still
  # waiting for review (the other architecture's, say) is only the entry to
  # build on.
  if jq -e --arg pkg "$package_id" '[.entries[] | select(.package_id == $pkg)] | length > 0' \
      "$snapshot" >/dev/null; then
    action=update
    [[ -s $previous ]] || jq --arg pkg "$package_id" \
      '.entries[] | select(.package_id == $pkg)' "$snapshot" > "$previous"
  else
    action=add
  fi
  args=(entry --arch "$arch" --identity "$REGISTRY_IDENTITY" --metadata "$metadata"
    --url "$REGISTRY_URL/artifacts/$OWNER/generic/$artifact/$VERSION/${tapp##*/}"
    --objects "$dir" --action "$action"
    # Each runtime build is hundreds of megabytes the registry keeps; an app
    # version whose build has been pruned is refused, not broken.
    --keep-releases 5)
  [[ -s $previous ]] && args+=(--previous-entry "$previous")
  "$REGISTRY_CLI" "${args[@]}" "$tapp" > "$request"
  status=$(curl --silent --show-error --connect-timeout 10 --max-time 60 \
    --retry 3 --retry-all-errors \
    --output "$response" --write-out '%{http_code}' --request POST \
    --header 'Content-Type: application/json' --data-binary "@$request" \
    "$REGISTRY_URL/entries")
  if [[ $status == 202 ]]; then
    echo "::notice::submitted $provides ($arch) to the Tilo registry as $package_id"
    exit 0
  fi
  printf 'registry answered %s (%s) with HTTP %s: %s\n' \
    "$package_id" "$arch" "$status" "$(head -c 1000 "$response")" >&2
  [[ $status == 409 || $status == 429 || $status == 5?? ]] || exit 1
  sleep "$attempt"
done
printf 'registry did not accept %s after bounded retries\n' "$provides" >&2
exit 1
