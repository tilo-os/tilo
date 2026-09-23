#!/usr/bin/env bash
# Export the schema-2 internal installer bundle from a built live installer disk
# and describe it for the release catalog.
#
#   ./os/dev/export-internal-bundle.sh DISK ARCH RELEASE_ID OUTDIR
#
#     DISK        os/output/installer/qcow2/disk.qcow2 (or a raw image)
#     ARCH        x86_64 | aarch64 (must match the live UKI)
#     RELEASE_ID  recorded in the manifest; the catalog requires the release tag
#     OUTDIR      new directory; receives
#                   bundle/tilo-internal-installer-ARCH{.json,-esp.img,-root.img}
#                   internal-catalog-ARCH.json   (one CatalogRelease::Internal entry)
#
# The export itself needs root (see os/dev/internal-bundle.py): this runs it
# through `sudo -n` unless already root, then hands the bundle back to the
# calling user and re-inspects it unprivileged. The catalog entry points at the
# Forgejo generic package that apps/imager/packaging/publish-internal-bundle.sh
# uploads:
#
#   https://code.podesta.ai/api/packages/OWNER/generic/tilo-internal-installer/RELEASE_ID/NAME
#
# OWNER is $TILO_INTERNAL_BUNDLE_OWNER (default qos). The origin is fixed: the
# Imager accepts internal manifests only from https://code.podesta.ai.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
die() { printf 'export-internal-bundle: %s\n' "$*" >&2; exit 1; }
[ "$#" -eq 4 ] || { sed -n '2,24p' "$0" >&2; exit 2; }
disk="$1" arch="$2" release="$3" out="$4"
owner="${TILO_INTERNAL_BUNDLE_OWNER:-qos}"
case "$arch" in x86_64|aarch64) ;; *) die "unsupported architecture: $arch" ;; esac
[[ "$release" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] || die "invalid release identity: $release"
[[ "$owner" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$ ]] || die "invalid package owner: $owner"
[ -f "$disk" ] && [ ! -L "$disk" ] || die "not a regular disk image: $disk"
[ ! -e "$out" ] || die "output must not exist: $out"

mkdir -p "$out"
exporter="$repo/os/dev/internal-bundle.py"
as_root=()
[ "$(id -u)" = 0 ] || as_root=(sudo -n)
${as_root[@]+"${as_root[@]}"} python3 "$exporter" export --image "$disk" --arch "$arch" --release "$release" \
  --output "$out/bundle" >"$out/export.json" \
  || die "export failed; see the message above"
${as_root[@]+"${as_root[@]}"} chown -R "$(id -u):$(id -g)" "$out/bundle"

manifest="$out/bundle/tilo-internal-installer-$arch.json"
python3 "$exporter" inspect --manifest "$manifest" >/dev/null \
  || die "exported bundle does not inspect clean as $(id -un)"
python3 - "$manifest" "$arch" "$release" "$owner" "$out/internal-catalog-$arch.json" <<'PY'
import hashlib, json, os, sys
manifest, arch, release, owner, fragment = sys.argv[1:]
raw = open(manifest, 'rb').read()
data = json.loads(raw)
if (data['architecture'], data['release_id']) != (arch, release):
    raise SystemExit('manifest does not describe the requested release')
entry = {
    'purpose': 'tilo-internal-installer',
    'layout': 'internal-bundle-v2',
    'release_id': release,
    'arch': arch,
    'manifest_url': 'https://code.podesta.ai/api/packages/%s/generic/tilo-internal-installer/%s/%s'
                    % (owner, release, os.path.basename(manifest)),
    'manifest_sha256': hashlib.sha256(raw).hexdigest(),
    'manifest_size': len(raw),
}
with open(fragment, 'x', encoding='utf-8') as output:
    json.dump(entry, output, indent=2, sort_keys=True)
    output.write('\n')
PY
rm -f "$out/export.json"
printf '%s\n' "$out/internal-catalog-$arch.json"
