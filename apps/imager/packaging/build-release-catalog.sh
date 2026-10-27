#!/usr/bin/env bash
# Merge architecture/profile-specific, digest-bound installer metadata into the exact
# catalog embedded by all native Imager builds.
#
# Media fragments are `catalog-*.json` (package-install-medium.sh). Internal
# installer fragments are `internal-catalog-*.json` (os/dev/export-internal-bundle.sh),
# each shipped beside the exact manifest it names; they become
# CatalogRelease::Internal entries after that manifest's bytes, schema and
# identity are checked. TILO_REQUIRE_INTERNAL_BUNDLE=1 requires one internal
# entry for every generic media architecture. Target policy fragments are
# `target-policy-catalog-*.json` (apps/imager/packaging/export-target-policy.py),
# each beside the exact signed archive it names; the producer re-verifies the
# archive, architecture, generation and both release-authority signatures.
set -euo pipefail
: "${GITHUB_REF_NAME:?missing release tag}"
release_repository="${TILO_RELEASE_REPOSITORY:-tilo-os/releases}"
release_server="${TILO_RELEASE_SERVER:-https://github.com}"
internal_owner="${TILO_INTERNAL_BUNDLE_OWNER:-tilo}"
require_internal="${TILO_REQUIRE_INTERNAL_BUNDLE:-0}"
[[ "$release_repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'release repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$release_server" =~ ^https://[^/]+$ ]] \
  || { printf 'release server must be an HTTPS origin\n' >&2; exit 2; }
[[ "$internal_owner" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$ ]] \
  || { printf 'internal bundle owner must be a Forgejo user or organisation name\n' >&2; exit 2; }
[[ "$require_internal" =~ ^[01]$ ]] \
  || { printf 'TILO_REQUIRE_INTERNAL_BUNDLE must be 0 or 1\n' >&2; exit 2; }
release_prefix="${release_server}/${release_repository}/releases/download/${GITHUB_REF_NAME}/"
# The Imager accepts internal manifests from this origin only (release.rs).
internal_prefix="https://code.tilo.so/api/packages/${internal_owner}/generic/tilo-internal-installer/${GITHUB_REF_NAME}/"
root="${1:-release-assets}"
out="${2:-dist/tilo-installer-catalog}"
mkdir -p "$out"
mapfile -t fragments < <(find "$root" -type f -name 'catalog-*.json' | sort)
[ "${#fragments[@]}" -ge 2 ] || { printf 'expected the two generic installer catalog fragments\n' >&2; exit 1; }
mapfile -t internal < <(find "$root" -type f -name 'internal-catalog-*.json' | sort)
mapfile -t target_policies < <(find "$root" -type f -name 'target-policy-catalog-*.json' | sort)
catalog="$(mktemp "$out/.catalog.XXXXXX")"
trap 'rm -f "$catalog"' EXIT

# Each internal fragment must name the manifest shipped beside it, byte for
# byte, and that manifest must be a valid schema-2 bundle for this release.
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
for fragment in ${internal[@]+"${internal[@]}"}; do
  python3 - "$fragment" "$internal_prefix" "$GITHUB_REF_NAME" "$repo/os/dev/internal-bundle.py" <<'PY'
import hashlib, importlib.util, json, sys
from pathlib import Path
fragment, prefix, tag, exporter = sys.argv[1:]
entry = json.loads(Path(fragment).read_text(encoding='utf-8'))
expected = {'purpose', 'layout', 'release_id', 'arch', 'manifest_url', 'manifest_sha256', 'manifest_size'}
if not isinstance(entry, dict) or set(entry) != expected:
    raise SystemExit(f'{fragment}: internal catalog entry has missing or unknown fields')
name = f"tilo-internal-installer-{entry['arch']}.json"
if (entry['purpose'], entry['layout'], entry['release_id']) != ('tilo-internal-installer', 'internal-bundle-v2', tag) \
        or entry['arch'] not in ('x86_64', 'aarch64') or entry['manifest_url'] != prefix + name:
    raise SystemExit(f'{fragment}: internal catalog entry does not describe {prefix}{name}')
matches = [path for path in Path(fragment).parent.rglob(name) if path.is_file() and not path.is_symlink()]
if len(matches) != 1:
    raise SystemExit(f'{fragment}: expected exactly one {name} beside it')
raw = matches[0].read_bytes()
if (len(raw), hashlib.sha256(raw).hexdigest()) != (entry['manifest_size'], entry['manifest_sha256']):
    raise SystemExit(f'{fragment}: manifest bytes do not match the catalog entry')
spec = importlib.util.spec_from_file_location('internal_bundle', exporter)
bundle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bundle)
manifest = bundle.parse_manifest(raw)
if (manifest['release_id'], manifest['architecture']) != (tag, entry['arch']):
    raise SystemExit(f'{fragment}: manifest identity does not match the catalog entry')
PY
done

for fragment in ${target_policies[@]+"${target_policies[@]}"}; do
  python3 - "$fragment" "$GITHUB_REF_NAME" <<'PY'
import json, re, sys
from pathlib import Path
fragment, tag = sys.argv[1:]
entry = json.loads(Path(fragment).read_text(encoding='utf-8'))
expected = {'purpose', 'layout', 'release_id', 'arch', 'generation',
            'target_uki_sha256', 'url', 'archive_sha256', 'archive_size'}
if not isinstance(entry, dict) or set(entry) != expected:
    raise SystemExit(f'{fragment}: target policy entry has missing or unknown fields')
if (entry['purpose'], entry['layout'], entry['release_id']) != ('tilo-target-policy', 'target-policy-bundle-v1', tag):
    raise SystemExit(f'{fragment}: target policy entry does not describe {tag}')
if entry['arch'] not in ('x86_64', 'aarch64') or type(entry['generation']) is not int or entry['generation'] <= 0:
    raise SystemExit(f'{fragment}: target policy architecture or generation is invalid')
for field in ('target_uki_sha256', 'archive_sha256'):
    if not isinstance(entry[field], str) or not re.fullmatch(r'[0-9a-f]{64}', entry[field]):
        raise SystemExit(f'{fragment}: invalid target policy {field}')
if type(entry['archive_size']) is not int or entry['archive_size'] <= 0:
    raise SystemExit(f'{fragment}: invalid target policy archive size')
if not entry['url'].startswith('https://code.tilo.so/'):
    raise SystemExit(f'{fragment}: target policy URL is not on the trusted package origin')
PY
  python3 "$repo/apps/imager/packaging/export-target-policy.py" verify \
    --bundle "$(dirname "$fragment")" --release-id "$GITHUB_REF_NAME" \
    || { printf '%s: target policy archive does not match its signed fragment\n' "$fragment" >&2; exit 1; }
done

jq -s --arg tag "$GITHUB_REF_NAME" \
  '(map(select(.purpose != "tilo-internal-installer" and .purpose != "tilo-target-policy"))
    | map(if has("hardware_profile") then . else . + {hardware_profile:"generic"} end)) as $media |
   (map(select(.purpose == "tilo-target-policy"))) as $target |
   (map(select(.purpose == "tilo-internal-installer"))) as $internal |
   {schema:(if any($media[]; .hardware_profile != "generic") then 2 else 1 end),
    tag:$tag,releases:(($media | sort_by(.arch,.hardware_profile)) + ($target | sort_by(.arch)) + ($internal | sort_by(.arch)))}' \
  "${fragments[@]}" ${target_policies[@]+"${target_policies[@]}"} ${internal[@]+"${internal[@]}"} >"$catalog"

jq -e --arg prefix "$release_prefix" --arg internal_prefix "$internal_prefix" \
  --argjson require_internal "$require_internal" '
  ([.releases[] | select(.purpose != "tilo-internal-installer" and .purpose != "tilo-target-policy")]) as $media |
  ([.releases[] | select(.purpose == "tilo-target-policy")]) as $target |
  ([.releases[] | select(.purpose == "tilo-internal-installer")]) as $internal |
  (.schema == 1 or .schema == 2) and
  (.tag | test("^imager-v[0-9]+\\.[0-9]+\\.[0-9]+$")) and
  ([$media[] | select(.hardware_profile == "generic") | .arch] | sort == ["aarch64", "x86_64"]) and
  ([$media[] | [.arch,.hardware_profile]] | length == (unique | length)) and
  all($media[];
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
      (.provenance.target_update_ref | test("^(registry|code)\\.tilo\\.so/tilo/tilo(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})$"))
    end)) and
  ([$target[] | .arch] | length == (unique | length)) and
  all($target[];
    (keys == ["arch","archive_sha256","archive_size","generation","layout","purpose","release_id","target_uki_sha256","url"]) and
    .layout == "target-policy-bundle-v1" and
    (.arch == "aarch64" or .arch == "x86_64") and
    .release_id == $tag and
    (.generation | type == "number" and . > 0) and
    (.target_uki_sha256 | test("^[0-9a-f]{64}$")) and
    (.archive_sha256 | test("^[0-9a-f]{64}$")) and
    .archive_size > 0 and
    (.url | startswith("https://code.tilo.so/"))) and
  ([$internal[] | .arch] | length == (unique | length)) and
  all($internal[];
    (keys == ["arch","layout","manifest_sha256","manifest_size","manifest_url","purpose","release_id"]) and
    .layout == "internal-bundle-v2" and
    (.arch == "aarch64" or .arch == "x86_64") and
    .release_id == $tag and
    .manifest_url == ($internal_prefix + "tilo-internal-installer-" + .arch + ".json") and
    (.manifest_sha256 | test("^[0-9a-f]{64}$")) and
    .manifest_size > 0 and .manifest_size <= 65536) and
  (if $require_internal == 1 then ([$internal[] | .arch] | sort == ["aarch64", "x86_64"]) else true end)
' --arg tag "$GITHUB_REF_NAME" "$catalog" >/dev/null
schema="$(jq -r .schema "$catalog")"
name="tilo-installer-catalog-v${schema}.json"
other="tilo-installer-catalog-v$((3-schema)).json"
[ ! -e "$out/$other" ] || { printf 'use a fresh output directory when changing catalog schema\n' >&2; exit 1; }
mv "$catalog" "$out/$name"
(cd "$out" && sha256sum "$name") >"$out/$name.sha256"
printf '%s\n' "$out/$name"
