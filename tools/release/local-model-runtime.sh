#!/usr/bin/env bash
# Builds the runtimes the signed local model catalog ships (crates/agent/local-model-service/
# RELEASE.md, "Signed runtimes"), as t's checks run them (t.toml, "Local model runtimes"). A
# build holds no secrets and writes OUT/<id>.oci.tar, an OCI archive whose index names the image,
# so `podman load` gives it the name and digest the catalog signs, and OUT/runtime.json, the
# runtime's entry for dist/local-models/catalog-source.json with the archive's receipt.
#
#   build ID OUT   build runtime ID into OUT/ and check that its server starts
#   ids            list the runtime ids
#
# The llama.cpp builds run in the buildah image, vLLM's in the skopeo image.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
source "$repo_root/tools/ci/publish/lib.sh"
cd "$repo_root"
runtimes=dist/local-models/runtimes

# Pins. The llama.cpp source is a release build's commit, as GitHub's archive of it.
llama_build=11783
llama_commit=339f3242c761f16c27e5ffcde1d35dcc31933a55
llama_sha256=901a2a2b2eb389a08cae3fec3269914f5c03e8d39b1352db9a29cbd1886727f3
fedora_minimal=registry.tilo.so/tilo/fedora-minimal@sha256:160ca210f19cb881f9f5d56211db3a0d7f41f531fda7e0df03410d72ba8ffd80
# NVIDIA's CUDA bases, their linux/amd64 images.
cuda_devel=registry.tilo.so/tilo/cuda@sha256:e542739fcaa4f45da5add8c4cf5769783a61628b2518304a5bbe4ace468b8c8f
cuda_runtime=registry.tilo.so/tilo/cuda@sha256:1199283a7fd5fc7dcb472ea4c6b6d8eb378ef8c0b9aae5e731491c01a219ca43
ubuntu_snapshot=20261020T000000Z
# vllm/vllm-openai v0.32.0, its linux/amd64 image.
vllm_version=0.32.0
vllm_image=docker.io/vllm/vllm-openai@sha256:befccf0e7e4089aa23926bb6f887a85c0d46c2a99e6de5c5ffdf5cb7957c5b34

package_base=https://code.tilo.so/api/packages/tilo/generic
ids=(llama-cpp-cpu-x86_64 llama-cpp-cpu-aarch64 llama-cpp-vulkan-x86_64 llama-cpp-vulkan-aarch64
  llama-cpp-cuda-x86_64 vllm-cuda-x86_64)

# The signed launch settings of each engine: the context the runtime allows (a model's own is
# the other bound) and, for llama.cpp, the layers it offloads (all of them on a GPU).
execution() {
  case $1 in
    llama-cpp-cpu-*) printf '{"engine":"llama_cpp","max_context_tokens":8192,"gpu_layers":0}' ;;
    llama-cpp-*) printf '{"engine":"llama_cpp","max_context_tokens":8192,"gpu_layers":999}' ;;
    vllm-*) printf '{"engine":"vllm","max_context_tokens":8192,"cpu_offload_gib":0}' ;;
  esac
}

kind() {
  case $1 in
    *-cpu-*) echo cpu ;;
    *-vulkan-*) echo vulkan ;;
    *-cuda-*) echo nvidia_cuda ;;
  esac
}

platform() {
  case ${1##*-} in
    x86_64) echo linux/amd64 ;;
    aarch64) echo linux/arm64 ;;
  esac
}

fetch_llama_source() {
  local dest=$1
  curl --fail --silent --show-error --location --proto '=https' --retry 3 \
    --output "$dest/llama.cpp.tar.gz" "https://github.com/ggml-org/llama.cpp/archive/$llama_commit.tar.gz"
  echo "$llama_sha256  $dest/llama.cpp.tar.gz" | sha256sum --check --quiet
}

# Name the image an OCI archive holds: rewrite its index with the reference annotation podman
# load reads. The manifest, and so the digest, stays as built.
name_archive() {
  local archive=$1 name=$2 dir manifest
  dir=$(mktemp -d "$scratch/index.XXXXXX")
  tar -xOf "$archive" index.json > "$dir/built.json"
  manifest=$(tr -d ' \n' < "$dir/built.json" | grep -o '{"mediaType":"application/vnd.oci.image.manifest.v1+json","digest":"sha256:[0-9a-f]\{64\}","size":[0-9]*' | head -1)
  [[ -n $manifest ]] || die "no manifest in $archive's index"
  printf '{"schemaVersion":2,"manifests":[%s,"annotations":{"org.opencontainers.image.ref.name":"%s"}}]}' \
    "$manifest" "$name" > "$dir/index.json"
  tar --delete -f "$archive" index.json
  tar --append -f "$archive" -C "$dir" --mtime=@0 --owner=0 --group=0 --numeric-owner index.json
  rm -rf "$dir"
}

build_llama() {
  local id=$1 out=$2 context
  context=$(mktemp -d "$scratch/llama-context.XXXXXX")
  fetch_llama_source "$context"
  local common=(--platform "$(platform "$id")"
    --build-arg "LLAMA_BUILD_NUMBER=$llama_build" --build-arg "LLAMA_BUILD_COMMIT=${llama_commit:0:7}")
  case $id in
    llama-cpp-cuda-*)
      bash tools/ci/publish/oci-build.sh "$context" "$runtimes/llama-cpp-cuda.Containerfile" "$out" "${common[@]}" \
        --build-arg "BUILD_BASE=$cuda_devel" --build-arg "RUN_BASE=$cuda_runtime" \
        --build-arg "UBUNTU_SNAPSHOT=$ubuntu_snapshot"
      ;;
    *)
      bash tools/ci/publish/oci-build.sh "$context" "$runtimes/llama-cpp.Containerfile" "$out" "${common[@]}" \
        --build-arg "BASE=$fedora_minimal" --build-arg "BACKEND=$(kind "$id")"
      ;;
  esac
  rm -rf "$context"
  # The server and every backend it loads resolve their libraries.
  bash tools/ci/publish/oci-run.sh "$out/image.oci.tar" /app/llama-server --version
  bash tools/ci/publish/oci-run.sh "$out/image.oci.tar" /app/llama-server --list-devices
  mv "$out/image.oci.tar" "$out/$id.oci.tar"
  name_archive "$out/$id.oci.tar" "registry.tilo.so/tilo/local-model-runtime-$id:b$llama_build"
  base_digest=$fedora_minimal
  [[ $id == *-cuda-* ]] && base_digest=$cuda_runtime
  version=b$llama_build
}

build_vllm() {
  local id=$1 out=$2
  mkdir -p "$out"
  # Only the manifest's media types change on the way to OCI; the layers are upstream's bytes.
  skopeo copy --retry-times 3 --format oci "docker://$vllm_image" \
    "oci-archive:$out/$id.oci.tar:registry.tilo.so/tilo/local-model-runtime-$id:$vllm_version"
  base_digest=$vllm_image
  version=$vllm_version
}

write_runtime() {
  local id=$1 out=$2 digest sha size
  digest=$(archive_digest "$out/$id.oci.tar")
  [[ $digest =~ ^sha256:[0-9a-f]{64}$ ]] || die "no manifest digest in $id's archive"
  sha=$(sha256sum "$out/$id.oci.tar" | cut -d' ' -f1)
  size=$(stat --format=%s "$out/$id.oci.tar")
  printf '%s\n' "$digest" > "$out/digest"
  cat > "$out/runtime.json" <<JSON
{
  "id": "$id",
  "kind": "$(kind "$id")",
  "version": "$version",
  "oci_image": "registry.tilo.so/tilo/local-model-runtime-$id@$digest",
  "execution": $(execution "$id"),
  "artifact": {
    "file": "runtimes/$id.oci.tar",
    "url": "$package_base/local-model-runtime-$id/$sha/$id.oci.tar",
    "sha256": "$sha",
    "size_bytes": $size
  },
  "base_image_digest": "${base_digest##*@}",
  "architecture": "${id##*-}",
  "kernel_abi": null,
  "requires_reboot": false
}
JSON
  printf 'built %s: %s, %s bytes, sha256 %s\n' "$id" "$digest" "$size" "$sha"
}

command=${1:-}
shift || true
case $command in
  build)
    id=${1:?usage: local-model-runtime.sh build ID OUT} out=${2:?}
    [[ " ${ids[*]} " == *" $id "* ]] || die "unknown runtime $id"
    case $id in
      llama-cpp-*) build_llama "$id" "$out" ;;
      vllm-*) build_vllm "$id" "$out" ;;
    esac
    write_runtime "$id" "$out"
    ;;
  ids)
    printf '%s\n' "${ids[@]}"
    ;;
  *)
    die "usage: local-model-runtime.sh build ID OUT | ids"
    ;;
esac
