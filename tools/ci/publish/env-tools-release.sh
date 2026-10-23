#!/usr/bin/env bash
# The env tool catalog's release (docs/publishing-on-t.md): on stable, the tools env-tools.sh
# built, sealed for the publisher without its key, are signed with TAPP_PUBLISHER_IDENTITY by a
# step that builds nothing, put in one image for both platforms, signed with the release key and
# published once as registry.tilo.so/tilo/env-tools:<runtime tag>-<catalog fingerprint>, which
# devices and relays read tools from (tilo-oci-images).
#
#   env-tools-release.sh plan OUT        what to build. OUT/tag is the tag the catalog publishes
#                                        as; OUT/published says it exists already and nothing is
#                                        built; otherwise OUT/runtime is the published
#                                        env-runtime, fetched and verified (CI builder)
#   env-tools-release.sh seal PLAN BUILDS BINARIES OUT
#                                        check both architectures' tools and sign them with the
#                                        publisher identity into OUT, each catalog naming the
#                                        signed ids (`tilo-env-tools sign-catalog`, CI builder)
#   env-tools-release.sh image SEALED OUT PLAN
#                                        the published image: SEALED for both platforms, joined
#                                        into one index (buildah image)
#   env-tools-release.sh preview BUILDS OUT
#                                        the image of the unsigned builds, a working tree's
#                                        proof that the image builds (buildah image)
#   env-tools-release.sh sign PLAN IMAGE sign the image's digest (oci-sign.sh, skopeo image)
#   env-tools-release.sh release PLAN IMAGE
#                                        publish it once as the plan's tag (skopeo image)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
cd "$repo_root"
repository=registry.tilo.so/tilo/env-tools
arches=(x86_64 aarch64)

# The first 16 hex digits of what decides the catalog's contents: the list and the builder
# sources that shape an image. The runtime is in the tag beside it.
catalog_fingerprint() {
  cat os/env-tools-catalog.txt os/Containerfile.env-tools \
    crates/runtime/env-tools/src/{key,seal,sandbox,builder,image,resolve}.rs \
    | sha256sum | cut -c1-16
}

# Exit the step when the plan found the catalog published already.
published_already() {
  if [[ -s $1/published ]]; then
    printf '%s:%s is published already; nothing to do\n' "$repository" "$(cat "$1/published")"
    exit 0
  fi
}

# OUT/image.oci.tar and OUT/digest: SEALED's tools, catalogs and reports as an image, one per
# platform, joined into one index. Every platform holds both architectures' tools, as devices look a tool up by
# its id wherever it was published.
build_image() {
  local sealed=$1 out=$2 platform
  for platform in linux/amd64 linux/arm64; do
    bash tools/ci/publish/oci-build.sh "$sealed" os/Containerfile.env-tools "$out/${platform#linux/}" \
      --platform "$platform" --build-context "sealed=$sealed"
  done
  python3 tools/ci/publish/oci-index.py "$out" "$out/amd64/image.oci.tar" "$out/arm64/image.oci.tar"
  rm -rf "$out/amd64" "$out/arm64"
}

case ${1:-} in
  plan)
    out=${2:?usage: env-tools-release.sh plan OUT}
    rm -rf "$out"
    mkdir -p "$out"
    dnf install -y --setopt=install_weak_deps=False skopeo >/dev/null
    use_cosign
    tag=$(bash os/dev/resolve-env-runtime.sh --tag)-$(catalog_fingerprint)
    [[ $tag =~ ^set-[0-9a-f]{64}-[0-9a-f]{16}$ ]] || die "not a catalog tag: $tag"
    printf '%s\n' "$tag" > "$out/tag"
    # Read anonymously, as devices read it.
    if skopeo inspect --raw "docker://$repository:$tag" > /dev/null 2> "$scratch/plan-error"; then
      printf '%s\n' "$tag" > "$out/published"
      printf '%s:%s is published already: nothing is built\n' "$repository" "$tag"
      exit 0
    fi
    grep -qE 'manifest unknown|name unknown|not found' "$scratch/plan-error" \
      || die "could not ask the registry for $repository:$tag: $(tail -n 1 "$scratch/plan-error")"
    # On stable the env-runtime was released before this step (release-env-runtime), so a runtime
    # that isn't there is an error, not a reason to build on a preview.
    status=0
    bash os/dev/fetch-env-runtime.sh "$out/runtime" || status=$?
    case $status in
      0) ;;
      1) die "no env-runtime is published as $(bash os/dev/resolve-env-runtime.sh --tag): the catalog is built for a published runtime only" ;;
      *) die 'could not fetch the published env-runtime' ;;
    esac
    printf 'building %s:%s\n' "$repository" "$tag"
    ;;
  seal)
    plan=${2:?usage: env-tools-release.sh seal PLAN BUILDS BINARIES OUT}
    builds=${3:?}
    binaries=${4:?}
    out=${5:?}
    published_already "$plan"
    [[ -n ${TAPP_PUBLISHER_IDENTITY:-} ]] || die 'TAPP_PUBLISHER_IDENTITY is required to seal the catalog'
    identity=$scratch/env-tools-publisher.json
    (umask 077; printf '%s' "$TAPP_PUBLISHER_IDENTITY" > "$identity")
    trap 'rm -f "$identity"' EXIT
    unset TAPP_PUBLISHER_IDENTITY
    publisher=$(cat "$plan/runtime/publisher.hex")
    [[ $publisher =~ ^[0-9a-f]{64}$ ]] || die "the published runtime names no publisher: $publisher"
    rm -rf "$out"
    mkdir -p "$out"
    for arch in "${arches[@]}"; do
      built=$builds/$arch/catalog
      runtime_id=$(cat "$plan/runtime/env-runtime-$arch.id")
      [[ $(cat "$built/runtime-source") == published && $(cat "$built/runtime.id") == "$runtime_id" ]] \
        || die "the $arch tools were not built on the published runtime $runtime_id"
      work=$scratch/env-tools-signed-$arch
      rm -rf "$work"
      "$binaries/tilo-env-tools" sign-catalog --identity "$identity" --runtime "$runtime_id" \
        --arch "$arch" --catalog "$built/catalog-$arch.json" --tapps "$built" --out "$work" \
        > "$scratch/signed-$arch.json"
      count=0
      for tapp in "$work"/*.tapp; do
        "$binaries/tilo-app-package" verify --trusted-key "$publisher" "$tapp" >/dev/null \
          || die "$(basename "$tapp") does not verify against the publisher $publisher"
        mv "$tapp" "$out/"
        count=$((count + 1))
      done
      mv "$work/catalog.json" "$out/catalog-$arch.json"
      # The report names each tool as the build did: name the signed ids instead.
      python3 - "$scratch/signed-$arch.json" "$built/report-$arch.json" "$out/report-$arch.json" <<'PY'
import json, sys
signed = json.load(open(sys.argv[1]))["signed"]
rows = json.load(open(sys.argv[2]))
for row in rows:
    row["tool"] = signed[row["tool"]]
json.dump(rows, open(sys.argv[3], "w"), indent=2)
PY
      printf '%s: %s tools signed\n' "$arch" "$count"
    done
    (cd "$out" && sha256sum ./*.tapp > SHA256SUMS)
    ;;
  image)
    sealed=${2:?usage: env-tools-release.sh image SEALED OUT PLAN}
    out=${3:?}
    published_already "${4:?}"
    build_image "$sealed" "$out"
    ;;
  preview)
    builds=${2:?usage: env-tools-release.sh preview BUILDS OUT}
    out=${3:?}
    sealed=$scratch/env-tools-preview
    rm -rf "$sealed"
    mkdir -p "$sealed"
    for arch in "${arches[@]}"; do
      cp "$builds/$arch/catalog"/*.tapp "$builds/$arch/catalog/catalog-$arch.json" \
        "$builds/$arch/catalog/report-$arch.json" "$sealed/"
    done
    (cd "$sealed" && sha256sum ./*.tapp > SHA256SUMS)
    build_image "$sealed" "$out"
    ;;
  sign)
    plan=${2:?usage: env-tools-release.sh sign PLAN IMAGE}
    published_already "$plan"
    bash tools/ci/publish/oci-sign.sh "$repository" "${3:?}"
    ;;
  release)
    plan=${2:?usage: env-tools-release.sh release PLAN IMAGE}
    out=${3:?}
    published_already "$plan"
    tag=$(cat "$plan/tag")
    [[ $tag =~ ^set-[0-9a-f]{64}-[0-9a-f]{16}$ ]] || die "not a catalog tag: $tag"
    # Once: a catalog published for this runtime and these sources is never replaced.
    bash tools/ci/publish/oci-publish.sh release "$repository" "$out" "once:$tag"
    ;;
  *) die 'usage: env-tools-release.sh plan|seal|image|preview|sign|release ...' ;;
esac
