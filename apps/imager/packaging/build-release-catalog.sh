#!/usr/bin/env bash
# Merge architecture/profile-specific, digest-bound installer metadata into the exact
# catalog embedded by all native Imager builds.
set -euo pipefail
: "${GITHUB_REF_NAME:?missing release tag}"
release_repository="${TILO_RELEASE_REPOSITORY:-qos/releases}"
release_server="${GITHUB_SERVER_URL:-https://code.podesta.ai}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
release_prefix="${release_server}/${release_repository}/releases/download/${GITHUB_REF_NAME}/"
root="${1:-release-assets}"
out="${2:-dist/tilo-installer-catalog}"
mkdir -p "$out"
mapfile -t fragments < <(find "$root" -type f -name 'catalog-*.json' | sort)
[ "${#fragments[@]}" -ge 2 ] || { printf 'expected the two generic installer catalog fragments\n' >&2; exit 1; }
catalog="$(mktemp "$out/.catalog.XXXXXX")"
trap 'rm -f "$catalog"' EXIT
jq -s --arg tag "$GITHUB_REF_NAME" \
  'map(if has("hardware_profile") then . else . + {hardware_profile:"generic"} end) |
   {schema:(if any(.[]; .hardware_profile != "generic") then 2 else 1 end),
    tag:$tag,releases:(sort_by(.arch,.hardware_profile))}' "${fragments[@]}" >"$catalog"

jq -e --arg prefix "$release_prefix" '
  (.schema == 1 or .schema == 2) and
  (.tag | test("^imager-v[0-9]+\\.[0-9]+\\.[0-9]+$")) and
  ([.releases[] | select(.hardware_profile == "generic") | .arch] | sort == ["aarch64", "x86_64"]) and
  ([.releases[] | [.arch,.hardware_profile]] | length == (unique | length)) and
  all(.releases[];
    (.arch == "aarch64" or .arch == "x86_64") and
    (.hardware_profile | test("^[a-z0-9][a-z0-9-]{0,63}$")) and
    (.url | startswith($prefix)) and
    (.archive_sha256 | test("^[0-9a-f]{64}$")) and
    .archive_size > 0 and
    (.image_sha256 | test("^[0-9a-f]{64}$")) and
    .image_size > 0 and
    (if .hardware_profile == "generic" then true else
      (.provenance.source_image_id | test("^sha256:[0-9a-f]{64}$")) and
      (.provenance.input_qcow2_sha256 | test("^[0-9a-f]{64}$")) and
      (.provenance.hardware_profile_sha256 | test("^[0-9a-f]{64}$")) and
      (.provenance.target_update_ref | test("^code\\.podesta\\.ai/qos/tilo(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})$"))
    end))
' --arg tag "$GITHUB_REF_NAME" "$catalog" >/dev/null
schema="$(jq -r .schema "$catalog")"
name="tilo-installer-catalog-v${schema}.json"
other="tilo-installer-catalog-v$((3-schema)).json"
[ ! -e "$out/$other" ] || { printf 'use a fresh output directory when changing catalog schema\n' >&2; exit 1; }
mv "$catalog" "$out/$name"
(cd "$out" && sha256sum "$name") >"$out/$name.sha256"
printf '%s\n' "$out/$name"
