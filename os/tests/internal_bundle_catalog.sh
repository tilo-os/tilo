#!/usr/bin/env bash
# Prove build-release-catalog.sh emits CatalogRelease::Internal entries only
# for exact, valid schema-2 manifests published on code.podesta.ai.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
build="$repo/apps/imager/packaging/build-release-catalog.sh"
work="$(mktemp -d /var/tmp/tilo-internal-catalog-test.XXXXXX)"
trap 'find "$work" -depth -delete' EXIT
tag=imager-v1.2.3
media_prefix="https://downloads.example/tilo/public-downloads/releases/download/$tag"
internal_prefix="https://code.podesta.ai/api/packages/qos/generic/tilo-internal-installer/$tag"
fail() { printf 'FAIL  %s\n' "$*" >&2; exit 1; }

reset() {
  rm -rf "$work/assets" "$work/out"
  mkdir -p "$work/assets/media" "$work/assets/internal"
  for arch in x86_64 aarch64; do
    jq -n --arg arch "$arch" --arg url "$media_prefix/tilo-installer-linux-$arch.img.zst" \
      '{name:"Tilo",version:"1.2.3",channel:"stable",arch:$arch,url:$url,
        archive_sha256:("a" * 64),archive_size:123,image_sha256:("b" * 64),image_size:456}' \
      >"$work/assets/media/catalog-$arch.json"
  done
}

# A valid manifest for ARCH and the fragment os/dev/export-internal-bundle.sh
# would write for it, from the shared schema-2 fixture.
internal() {
  local arch="$1" dir="$work/assets/internal/$1"
  mkdir -p "$dir/bundle"
  python3 - "$repo/os/tests/fixtures/internal-bundle-v2.json" "$arch" "$tag" \
    "$dir/bundle/tilo-internal-installer-$arch.json" "$dir/internal-catalog-$arch.json" \
    "$internal_prefix" <<'PY'
import hashlib, json, sys
fixture, arch, tag, manifest, fragment, prefix = sys.argv[1:]
data = json.load(open(fixture, encoding='utf-8'))
data['release_id'] = tag
data['architecture'] = data['boot_binding']['uki_architecture'] = arch
data['boot_binding']['efi_path'] = {'x86_64': '/EFI/BOOT/BOOTX64.EFI', 'aarch64': '/EFI/BOOT/BOOTAA64.EFI'}[arch]
raw = (json.dumps(data, indent=2, sort_keys=True) + '\n').encode()
open(manifest, 'wb').write(raw)
name = manifest.rsplit('/', 1)[1]
json.dump({'purpose': 'tilo-internal-installer', 'layout': 'internal-bundle-v2', 'release_id': tag,
           'arch': arch, 'manifest_url': prefix + '/' + name,
           'manifest_sha256': hashlib.sha256(raw).hexdigest(), 'manifest_size': len(raw)},
          open(fragment, 'w', encoding='utf-8'))
PY
}

run() {
  GITHUB_REF_NAME="$tag" TILO_RELEASE_SERVER=https://downloads.example \
  TILO_RELEASE_REPOSITORY=tilo/public-downloads "$@" "$build" "$work/assets" "$work/out" >/dev/null 2>&1
}

reset
run env || fail 'catalog without internal bundles no longer builds'
jq -e '[.releases[] | select(.purpose == "tilo-internal-installer")] | length == 0' \
  "$work/out/tilo-installer-catalog-v1.json" >/dev/null || fail 'internal entries appeared from nowhere'
if run env TILO_REQUIRE_INTERNAL_BUNDLE=1; then fail 'required internal bundles were not required'; fi

reset
internal x86_64
internal aarch64
run env TILO_REQUIRE_INTERNAL_BUNDLE=1 || fail 'valid internal bundles were rejected'
jq -e --arg prefix "$internal_prefix/" '
  ([.releases[] | select(.purpose == "tilo-internal-installer")]) as $internal |
  ($internal | length == 2) and
  all($internal[]; (keys == ["arch","layout","manifest_sha256","manifest_size","manifest_url","purpose","release_id"])
    and .layout == "internal-bundle-v2" and (.manifest_url | startswith($prefix))
    and (has("hardware_profile") | not)) and
  ([.releases[] | select(.purpose != "tilo-internal-installer") | .hardware_profile] == ["generic","generic"])
' "$work/out/tilo-installer-catalog-v1.json" >/dev/null || fail 'internal catalog entries have the wrong shape'
printf 'ok    valid internal bundles become catalog entries\n'

for problem in bytes url tag schema-1 duplicate extra-field; do
  reset
  internal x86_64
  fragment="$work/assets/internal/x86_64/internal-catalog-x86_64.json"
  manifest="$work/assets/internal/x86_64/bundle/tilo-internal-installer-x86_64.json"
  case "$problem" in
    bytes) printf ' ' >>"$manifest" ;;
    url) jq '.manifest_url = "https://github.com/tilo-os/releases/releases/download/imager-v1.2.3/tilo-internal-installer-x86_64.json"' \
           "$fragment" >"$fragment.new" && mv "$fragment.new" "$fragment" ;;
    tag) jq '.release_id = "imager-v1.2.4"' "$fragment" >"$fragment.new" && mv "$fragment.new" "$fragment" ;;
    schema-1)
      python3 - "$manifest" "$fragment" <<'PY'
import hashlib, json, sys
manifest, fragment = sys.argv[1:]
data = json.load(open(manifest))
data['schema'] = 1
raw = json.dumps(data).encode()
open(manifest, 'wb').write(raw)
entry = json.load(open(fragment))
entry.update(manifest_sha256=hashlib.sha256(raw).hexdigest(), manifest_size=len(raw))
json.dump(entry, open(fragment, 'w'))
PY
      ;;
    duplicate) mkdir -p "$work/assets/internal/copy" && cp -R "$work/assets/internal/x86_64/." "$work/assets/internal/copy/" ;;
    extra-field) jq '.hardware_profile = "generic"' "$fragment" >"$fragment.new" && mv "$fragment.new" "$fragment" ;;
  esac
  if run env; then fail "catalog accepted an internal bundle with bad $problem"; fi
done
printf 'ok    internal catalog entries are bound to exact published manifests\n'
