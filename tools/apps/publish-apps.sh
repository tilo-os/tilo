#!/usr/bin/env bash
# Publish one architecture's signed app release (the sign-apps-<arch> check's
# output) to the Tilo registry at apps.tilo.so.
#
#   tools/apps/publish-apps.sh RELEASE_DIR [PACKAGE_CLI]
#
# RELEASE_DIR is what dist/apps/package-apps.sh wrote with the release
# identities: index.json and tapp/, runtime-package/ (the Tilo runtime) with
# runtime-package/tilolang/ (the tilolang runtime), language-packs/,
# work/receipt-signers.json, and the tilo-app-package that packed them, which
# runs the publication unless PACKAGE_CLI names one built for this machine.
#
# Runs only in a trusted run of main (T_REF, T_COMMIT), with two secrets:
# TILO_PACKAGES_TOKEN writes the immutable generic packages the registry's
# artifact proxy serves (code.tilo.so's tilo/generic), and
# TAPP_PUBLISHER_IDENTITY signs each registry entry. Both are taken out of the
# environment before anything else runs.
#
# In order: refuse unless the registry's catalogue is at the package format
# this commit reads and the release is signed by the keys devices already
# trust; upload every tapp with its compressed twin, the store artwork and the
# runtime packages' objects; then submit the runtimes, the apps and the
# language packs as publisher-signed entries. The registry's collector and its
# networkless signer verify each submission again before the signed catalogue
# names it. A package already published as these bytes is left alone, so a run
# that stopped part way is finished by running it again.
#
# T_INPUT_APPS, when set, names the apps (by their release name, `tl-chat`) to
# publish this time; the runtimes are always published, the language packs
# only with every app.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
# shellcheck source=/dev/null
source tools/ci/publish/lib.sh

release=$(realpath -e "${1:?usage: publish-apps.sh RELEASE_DIR [PACKAGE_CLI]}")
[[ $(channel) == dev ]] || die "apps are published from main, not ${T_REF}"
require_commit
: "${TILO_PACKAGES_TOKEN:?the package token is not set: the check needs secrets = [\"TILO_PACKAGES_TOKEN\"]}"
: "${TAPP_PUBLISHER_IDENTITY:?the publisher identity is not set: the check needs secrets = [\"TAPP_PUBLISHER_IDENTITY\"]}"
work=$(mktemp -d "$scratch/publish-apps.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
identity="$work/publisher.json"
(umask 077 && printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
export TILO_PACKAGE_TOKEN=$TILO_PACKAGES_TOKEN
unset TAPP_PUBLISHER_IDENTITY TILO_PACKAGES_TOKEN

registry=https://apps.tilo.so
owner=tilo
package_api=https://code.tilo.so/api/packages/$owner/generic
artifacts=$registry/artifacts/$owner/generic
version=0.0.0-$T_COMMIT
index=$release/index.json
cli=${2:-$release/tilo-app-package}
[[ -f $index && -x $cli ]] || die "$release holds no signed release (index.json, tilo-app-package)"
arch=$(jq -er '.architecture | select(. == "x86_64" or . == "aarch64")' "$index")
export RUNNER_TEMP=$work

# --- What the registry and devices accept ----------------------------------------------------
# A device reads one catalogue format, this commit's: a submission to a catalogue at another
# format lists builds no device built from this commit can verify.
format=$(sed -n 's/^pub const PACKAGE_FORMAT_VERSION: u32 = \([0-9]*\);$/\1/p' \
  crates/core/registry-types/src/lib.rs)
served=$(curl --fail --silent --show-error --max-time 120 "$registry/entries?format=compact" \
  | jq -er .protocol) || die "$registry serves no catalogue"
[[ -n $format && $served == "$format" ]] \
  || die "$registry publishes its catalogue at package format $served, but this commit reads format $format: deploy this commit's registry-server and sign a snapshot at format $format first (docs/registry.md)"

snapshot="$work/snapshot.json"
fetch_snapshot() {
  curl --fail --silent --show-error --connect-timeout 10 --max-time 60 \
    --retry 3 --retry-all-errors "$registry/entries" --output "$snapshot"
  jq -e '.entries | type == "array"' "$snapshot" >/dev/null
}
fetch_snapshot

# The keys the release was signed with, as the signing run recorded them, against the keys a
# device already trusts: the registry's allowlisted publishers, which the owner signs into every
# snapshot, for the packages, and os/registry-runtime-publisher.hex, which every image installs
# into /etc/tilo/runtime-bundle-trusted-publishers.txt, for the runtimes inside them.
signers=$release/work/receipt-signers.json
app_publisher=$(jq -er '.app_publisher | ascii_downcase' "$signers")
runtime_publisher=$(jq -er '.runtime_publisher | ascii_downcase' "$signers")
trusted_runtime=$(tr -d '[:space:]' < os/registry-runtime-publisher.hex)
[[ $runtime_publisher == "$trusted_runtime" ]] \
  || die "the release's runtimes are signed by $runtime_publisher, but devices trust $trusted_runtime for the registry's runtimes (os/registry-runtime-publisher.hex): no device would start them"
jq -e --arg key "$app_publisher" '[.publishers[].pubkey_hex | ascii_downcase] | index($key) != null' \
  "$snapshot" >/dev/null \
  || die "the release's packages are signed by $app_publisher, which $registry does not allowlist (it allowlists $(jq -r '[.publishers[].pubkey_hex] | join(", ")' "$snapshot"))"
printf 'publishing %s apps signed by %s, runtimes by %s, to %s at format %s\n' \
  "$arch" "$app_publisher" "$runtime_publisher" "$registry" "$format"

# --- The apps this run publishes -------------------------------------------------------------
plan="$work/apps.tsv"
jq -r '.apps[] | [.id, .name, .file] | @tsv' "$index" | LC_ALL=C sort > "$plan"
everything=true
if [[ -n ${T_INPUT_APPS:-} ]]; then
  everything=false
  read -r -a wanted <<<"${T_INPUT_APPS//,/ }"
  for name in "${wanted[@]}"; do
    awk -F'\t' -v name="$name" '$2 == name { found = 1 } END { exit !found }' "$plan" \
      || die "the release has no app $name (it has: $(cut -f2 "$plan" | xargs))"
  done
  awk -F'\t' -v names=" ${wanted[*]} " 'index(names, " " $2 " ")' "$plan" > "$plan.selected"
  mv "$plan.selected" "$plan"
fi

# --- Uploads ---------------------------------------------------------------------------------
# Beside each tapp goes its compressed twin, <file>.zst, which a device downloads first and
# expands, bounded by the signed size, before the signed hash check.
uploads="$work/uploads.tsv"
twins="$work/twins"
mkdir -p "$twins"
: > "$uploads"
while IFS=$'\t' read -r _id name file; do
  tapp="$release/tapp/$file"
  [[ -f $tapp ]] || die "the release index names $file, which is not in $release/tapp"
  "$cli" compress -o "$twins/$file.zst" "$tapp" >/dev/null
  printf '%s\t%s\n' "$tapp" "$package_api/tilo-app-$name/$version/$file" >> "$uploads"
  printf '%s\t%s\n' "$twins/$file.zst" "$package_api/tilo-app-$name/$version/$file.zst" >> "$uploads"
done < "$plan"

# The store's artwork and copy for each app, keyed by its id's last part (`so.tilo.chat` is
# `chat`). A package that is only something apps use, with no tag line, is listed without them.
metadata="$work/metadata"
media="$work/media"
mkdir -p "$metadata" "$media"
while IFS=$'\t' read -r id name _file; do
  key=${id#so.tilo.}
  if python3 -I tools/apps/registry_metadata.py build --apps "$key" \
      --media-base "$artifacts/tilo-app-metadata/$version" \
      --output "$work/metadata-one" --media-output "$media" 2>"$work/metadata.err"; then
    mv "$work/metadata-one/$key.json" "$metadata/$name.json"
  else
    printf '%s is listed without store copy: %s\n' "$id" "$(cat "$work/metadata.err")" >&2
  fi
done < "$plan"
while IFS= read -r file; do
  printf '%s\t%s\n' "$media/$file" "$package_api/tilo-app-metadata/$version/$file" >> "$uploads"
done < <(find "$media" -maxdepth 1 -type f -printf '%f\n' | LC_ALL=C sort)
TILO_PACKAGE_UPLOAD_WORKERS=8 tools/upload_immutable_packages.sh "$uploads"

# An entry signs its artwork's URLs, so the proxy must serve each of them, as an image, first.
while IFS= read -r file; do
  url="$artifacts/tilo-app-metadata/$version/$file"
  type=$(curl --fail --silent --show-error --location --head --retry 5 --retry-all-errors \
    --output /dev/null --write-out '%{content_type}' "$url") \
    || die "$url is not served"
  [[ $type == image/* ]] || die "$url is served as ${type:-nothing}, not as an image"
done < <(find "$media" -maxdepth 1 -type f -printf '%f\n')

# --- Submissions -----------------------------------------------------------------------------
response="$work/response.txt"
# POST one signed request; 0 when the registry took it, 2 when a retry may succeed.
submit() {
  local request=$1 status
  status=$(curl --silent --show-error --connect-timeout 10 --max-time 60 \
    --retry 3 --retry-all-errors --output "$response" --write-out '%{http_code}' \
    --request POST --header 'Content-Type: application/json' --data-binary "@$request" \
    "$registry/entries")
  [[ $status == 202 ]] && return 0
  printf 'the registry answered HTTP %s: %s\n' "$status" "$(head -c 1000 "$response")" >&2
  [[ $status == 409 || $status == 429 || $status == 5?? ]] && return 2
  return 1
}

# The runtimes first: the registry promotes an app only once the runtime it names is offered.
runtime_metadata() {
  jq -n --arg name "$1" --arg tagline "$2" --arg description "$3" \
    '{name: {en: $name}, description: {en: $description}, tagline: {en: $tagline},
      supported_languages: ["en"], source_url: "https://code.tilo.so/tilo/tilo"}' > "$4"
}
runtime_metadata "Tilo runtime" "Runtime for Tilo apps" \
  "The shared libraries Tilo apps are built against. The Store installs the build an app needs together with the app." \
  "$work/runtime.json"
runtime_metadata "Tilolang runtime" "Runtime for tilolang apps" \
  "The program every tilolang app runs on, with the standard library it was built against. The Store installs it together with the apps that need it." \
  "$work/tilolang.json"
publish_runtime() {
  VERSION=$version PACKAGE_API=$package_api REGISTRY_URL=$registry OWNER=$owner \
    REGISTRY_CLI=$cli REGISTRY_IDENTITY=$identity \
    tools/apps/publish-runtime-package.sh "$@"
}
failed=()
publish_runtime "$release/runtime-package" "$arch" so.tilo.runtime tilo-app-runtime \
  "$work/runtime.json" || failed+=(so.tilo.runtime)
if [[ -d $release/runtime-package/tilolang ]]; then
  publish_runtime "$release/runtime-package/tilolang" "$arch" so.tilo.runtime.tilolang \
    tilo-runtime-tilolang "$work/tilolang.json" || failed+=(so.tilo.runtime.tilolang)
fi

# One package id is one entry. A request still waiting for review is replaced by the next, so a
# new request builds on it (the other architecture's versions, say), or else on the published
# entry, whose history it keeps.
publish_entry() {
  local id=$1 tapp=$2 url=$3 arch=$4 listing=$5 attempt previous request action result
  shift 5
  for attempt in 1 2 3; do
    (( attempt == 1 )) || fetch_snapshot
    previous="$work/previous.json"
    request="$work/request.json"
    rm -f "$previous" "$request"
    "$cli" pending --registry "$registry" --identity "$identity" "$id" > "$previous"
    if jq -e --arg id "$id" '[.entries[] | select(.package_id == $id)] | length > 0' \
        "$snapshot" >/dev/null; then
      action=update
      [[ -s $previous ]] || jq --arg id "$id" '.entries[] | select(.package_id == $id)' \
        "$snapshot" > "$previous"
    else
      action=add
    fi
    local args=(entry --arch "$arch" --identity "$identity" --action "$action" --url "$url" "$@")
    [[ -n $listing ]] && args+=(--metadata "$listing")
    [[ -s $previous ]] && args+=(--previous-entry "$previous")
    "$cli" "${args[@]}" "$tapp" > "$request" || return 1
    if [[ -s $previous ]] && jq -e -s '.[0] == .[1].entry' "$previous" "$request" >/dev/null; then
      printf '%s (%s) is already current in the registry\n' "$id" "$arch"
      return 0
    fi
    result=0
    submit "$request" || result=$?
    if (( result == 0 )); then
      printf 'submitted %s (%s)\n' "$id" "$arch"
      return 0
    fi
    (( result == 2 )) || return 1
    sleep "$attempt"
  done
  return 1
}

while IFS=$'\t' read -r id name file; do
  listing=
  [[ -f $metadata/$name.json ]] && listing=$metadata/$name.json
  publish_entry "$id" "$release/tapp/$file" "$artifacts/tilo-app-$name/$version/$file" \
    "$arch" "$listing" || failed+=("$id")
done < "$plan"

# Every language other than English, as its language pack: data for every architecture, so one
# architecture's run publishes them (x86_64's), with the architecture `any`.
packs=$release/language-packs
if [[ $everything == true && $arch == x86_64 && -f $packs/index.json ]]; then
  : > "$uploads"
  while IFS=$'\t' read -r _id file _listing; do
    "$cli" compress -o "$twins/$file.zst" "$packs/$file" >/dev/null
    printf '%s\t%s\n' "$packs/$file" "$package_api/tilo-language-packs/$version/$file" >> "$uploads"
    printf '%s\t%s\n' "$twins/$file.zst" "$package_api/tilo-language-packs/$version/$file.zst" >> "$uploads"
  done < <(jq -r '.packs[] | [.id, .file, .metadata] | @tsv' "$packs/index.json")
  TILO_PACKAGE_UPLOAD_WORKERS=8 tools/upload_immutable_packages.sh "$uploads"
  while IFS=$'\t' read -r id file listing; do
    publish_entry "$id" "$packs/$file" "$artifacts/tilo-language-packs/$version/$file" any \
      "$packs/$listing" --keep-releases 3 || failed+=("$id")
  done < <(jq -r '.packs[] | [.id, .file, .metadata] | @tsv' "$packs/index.json")
fi

(( ${#failed[@]} == 0 )) || die "the registry did not take: ${failed[*]}"
printf 'every package is submitted; the registry lists each once its collector and signer have verified it\n'
