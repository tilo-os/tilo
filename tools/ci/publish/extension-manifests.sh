#!/usr/bin/env bash
# Downloadable system-extension manifests (tools/extensions, os/usr/share/tilo/system-extensions/
# README.md, "Downloaded system extensions") for the published OS images: t's
# extension-manifests-{x86_64,aarch64} checks resolve them, publish-extension-manifests signs
# and publishes them.
#
#   extension-manifests.sh resolve ARCH OUT   resolve every extension inside each published
#                                             OS image's ARCH child, natively: the image's own
#                                             rpm database is the base, its own dnf5 and rpmkeys
#                                             fetch and check upstream packages; holds no
#                                             credentials (as root in a micro-VM step)
#   extension-manifests.sh publish DIR...     sign each resolved manifest under DIR with the
#                                             extension-manifest key and publish it
#                                             (tools/extensions/publish.sh)
#
# The images: T_INPUT_IMAGES, space-separated (registry.tilo.so/tilo/{tilo,tilo-ci,tilo-vm} by
# :tag or @sha256:), else tilo:latest, tilo-vm:latest and tilo-vm:<arch>-ci, skipping those not
# published. T_INPUT_EXPECTED_CONTENT_SHA256, a reviewed media-codecs content hash
# (resolve.py content-sha256), binds one immutable image: only media-codecs is resolved, and
# its content must match. T_INPUT_PUBLISH=false qualifies that image without promoting the
# manifest into the client feed (--sign-only, which requires the hash); true, the default,
# promotes it, still checked against the hash when one is given.
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
package_base=https://code.tilo.so/api/packages/tilo/generic
expected=${T_INPUT_EXPECTED_CONTENT_SHA256:-}
promote=${T_INPUT_PUBLISH:-true}
[[ $promote == true || $promote == false ]] || die "publish must be true or false, not $promote"

case ${1:-} in
  resolve)
    arch=${2:?usage: extension-manifests.sh resolve ARCH OUT}
    out=${3:?}
    [[ $(uname -m) == "$arch" ]] || die "extensions resolve natively, not $arch on $(uname -m)"
    case $arch in x86_64) platform=amd64 ;; aarch64) platform=arm64 ;; *) die "no lane for $arch" ;; esac
    dnf install -y --setopt=install_weak_deps=False podman skopeo fuse-overlayfs jq python3 >/dev/null
    read -r -a images <<< "${T_INPUT_IMAGES:-}"
    explicit=true
    if ((${#images[@]} == 0)); then
      explicit=false
      images=(registry.tilo.so/tilo/tilo:latest registry.tilo.so/tilo/tilo-vm:latest "registry.tilo.so/tilo/tilo-vm:$arch-ci")
    fi
    if [[ $promote == false || -n $expected ]]; then
      [[ ${#images[@]} == 1 && ${images[0]} == *@sha256:* && $expected =~ ^[0-9a-f]{64}$ ]] \
        || die 'qualification requires one immutable image and a reviewed content hash'
    fi
    mkdir -p "$out"
    lanes=0
    for image in "${images[@]}"; do
      [[ $image =~ ^(registry\.tilo\.so/tilo/tilo(-ci|-vm)?)(:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}|@sha256:[0-9a-f]{64})$ ]] \
        || die "not a published OS image: $image"
      repository=${BASH_REMATCH[1]}
      if ! raw=$(skopeo inspect --raw "docker://$image" 2>/dev/null); then
        [[ $explicit == false ]] || die "$image does not exist"
        echo "$image is not published; skipping it"
        continue
      fi
      # This architecture's child: of an index, or the image itself.
      if jq -e .manifests <<< "$raw" >/dev/null; then
        digest=$(jq -r --arg p "$platform" '[.manifests[] | select(.platform.os == "linux" and .platform.architecture == $p)][0].digest // empty' <<< "$raw")
      else
        digest=sha256:$(printf '%s' "$raw" | sha256sum | cut -d' ' -f1)
        [[ $(skopeo inspect "docker://$repository@$digest" | jq -r .Architecture) == "$platform" ]] || digest=
      fi
      [[ -n $digest ]] || { echo "$image has no $arch child"; continue; }
      child=$repository@$digest
      key=$arch-${digest:7:16}
      [[ ! -d $out/$key ]] || continue
      mkdir -p "$out/$key"
      echo "$arch: $child ($image)"
      podman pull -q "$child" >/dev/null
      for recipe in tools/extensions/*/recipe.toml; do
        id=$(basename "$(dirname "$recipe")")
        [[ -z $expected || $id == media-codecs ]] || continue
        podman run --rm --network host --security-opt label=disable \
          --volume "$PWD/tools/extensions:/extensions:ro" --volume "$out/$key:/out" \
          --entrypoint python3 "$child" /extensions/resolve.py resolve \
          --recipe "/extensions/$id/recipe.toml" --image "$child" --out "/out/$id.json"
        qualification=()
        [[ -z $expected ]] || qualification=(--expected-content-sha256 "$expected")
        python3 tools/extensions/resolve.py check "$out/$key/$id.json" "${qualification[@]}"
      done
      # Prebuilt sysext images (the Windows compatibility payload) are pinned, not resolved:
      # the OS build publishes one with a descriptor per commit.
      if [[ -z $expected ]]; then
        revision=$(podman image inspect "$child" --format '{{ index .Config.Labels "org.opencontainers.image.revision" }}')
        if [[ $revision =~ ^[0-9a-f]{40}$ ]]; then
          descriptor=$scratch/descriptor-$key.json
          status=$(curl --silent --show-error --retry 3 --output "$descriptor" --write-out '%{http_code}' \
            "$package_base/tilo-windows-compat-image/$arch-$revision/descriptor.json")
          case $status in
            404) echo "no Windows compatibility system extension was published for $revision" ;;
            200)
              fingerprint=$(podman run --rm --security-opt label=disable \
                --volume "$PWD/tools/extensions:/extensions:ro" \
                --entrypoint python3 "$child" /extensions/resolve.py fingerprint)
              sha256=$(jq -er .sha256 "$descriptor")
              python3 tools/extensions/sysext_image.py manifest --id windows-compat --descriptor "$descriptor" \
                --url "$package_base/tilo-windows-compat-image/$arch-$sha256/tilo-windows-compat.raw" \
                --fingerprint "$fingerprint" --image "$child" --arch "$arch" --out "$out/$key/windows-compat.json"
              python3 tools/extensions/resolve.py check "$out/$key/windows-compat.json" ;;
            *) die "reading the Windows compatibility descriptor answered HTTP $status" ;;
          esac
        fi
      fi
      podman rmi -f "$child" >/dev/null 2>&1 || true
      lanes=$((lanes + 1))
    done
    echo "resolved $lanes $arch image(s)"
    ;;
  publish)
    shift
    (($#)) || die 'usage: extension-manifests.sh publish DIR...'
    use_cosign
    dnf install -y --setopt=install_weak_deps=False jq >/dev/null 2>&1 || command -v jq >/dev/null
    export TILO_PACKAGE_PUBLIC_BASE_URL=$package_base TILO_PACKAGE_TOKEN=${TILO_PACKAGES_TOKEN:-}
    export TILO_EXPECTED_EXTENSION_CONTENT_SHA256=$expected
    . os/dev/configure-cosign-kms.sh
    public_key=$scratch/extension-public-key.pem
    cosign public-key --key "$COSIGN_KEY_REF" > "$public_key" \
      || die 'could not read the extension-manifest public key from KMS'
    cmp --silent keys/extensions.pub "$public_key" || die 'the KMS extension-manifest key does not match keys/extensions.pub'
    mapfile -t manifests < <(find "$@" -type f -name '*.json' -print 2>/dev/null | LC_ALL=C sort)
    ((${#manifests[@]})) || die 'no lane resolved a manifest'
    mode=()
    if [[ $promote == false ]]; then
      [[ -n $expected ]] || die 'a sign-only run qualifies one image against a reviewed content hash'
      mode=(--sign-only)
    fi
    failed=0
    for manifest in "${manifests[@]}"; do
      bash tools/extensions/publish.sh "${mode[@]}" "$manifest" || failed=1
    done
    exit "$failed"
    ;;
  *) die 'usage: extension-manifests.sh resolve ARCH OUT | publish DIR...' ;;
esac
