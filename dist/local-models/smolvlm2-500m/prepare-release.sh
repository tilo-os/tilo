#!/usr/bin/env bash
# Produce the two real, content-addressed inputs to a Tilo local-model
# catalog.  This script deliberately does not sign or publish anything.
set -euo pipefail

readonly MODEL_ID='huggingface/smolvlm2-500m-video-instruct'
readonly MODEL_REVISION='a3992d502a8714ce2f6207fde3507ccf9503f224'
readonly MODEL_REPOSITORY='HuggingFaceTB/SmolVLM2-500M-Video-Instruct'
readonly RUNTIME_VERSION='0.10.1'
readonly RUNTIME_DIGEST='c62343ee72f094ef574ab3af2717d0c60fec5e0808b238f19ac4c0d22fd152f7'
readonly RUNTIME_IMAGE="docker.io/vllm/vllm-openai@sha256:${RUNTIME_DIGEST}"

usage() {
    cat <<'EOF'
Usage: prepare-release.sh --output DIR --publish-base-url HTTPS_URL --base-image-digest sha256:HEX

Fetches and verifies the exact Hugging Face snapshot and exact vLLM OCI
image, emits deterministic model/runtime archives, and writes local-models.json.
The supplied publish base must be the permanent HTTPS location where this
release's artifacts will be uploaded. This script never invents a digest.
EOF
}

output=''
publish_base=''
base_image_digest=''
while (($#)); do
    case "$1" in
        --output) output=${2:?missing output}; shift 2 ;;
        --publish-base-url) publish_base=${2:?missing publish URL}; shift 2 ;;
        --base-image-digest) base_image_digest=${2:?missing base digest}; shift 2 ;;
        --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[[ -n "$output" && -n "$publish_base" && -n "$base_image_digest" ]] || {
    usage >&2
    exit 2
}
[[ "$publish_base" =~ ^https://[^?#]+$ ]] || {
    echo 'publish base URL must be HTTPS and contain no query or fragment' >&2
    exit 2
}
[[ "$base_image_digest" =~ ^sha256:[0-9a-f]{64}$ ]] || {
    echo 'base image digest must be sha256 followed by 64 lowercase hex characters' >&2
    exit 2
}
for command in curl sha256sum skopeo tar zstd find awk stat; do
    command -v "$command" >/dev/null || {
        echo "required command is unavailable: $command" >&2
        exit 127
    }
done

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
output=$(mkdir -p "$output" && CDPATH= cd -- "$output" && pwd)
snapshot="$output/snapshot"
artifacts="$output/artifacts"
[[ ! -e "$snapshot" && ! -e "$artifacts" ]] || {
    echo 'output must not already contain snapshot or artifacts directories' >&2
    exit 2
}
mkdir -p "$snapshot" "$artifacts"

while IFS='  ' read -r expected file; do
    [[ -n "$expected" && -n "$file" ]] || continue
    destination="$snapshot/$file"
    mkdir -p "$(dirname -- "$destination")"
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 --output "$destination" \
        "https://huggingface.co/${MODEL_REPOSITORY}/resolve/${MODEL_REVISION}/${file}"
    actual=$(sha256sum "$destination" | awk '{print $1}')
    [[ "$actual" == "$expected" ]] || {
        echo "Hugging Face artifact digest mismatch: $file" >&2
        exit 1
    }
done < "$script_dir/model-files.sha256"

model_archive="$artifacts/smolvlm2-500m-video-instruct-${MODEL_REVISION}.tar.zst"
(
    cd "$snapshot"
    # Archive only regular, relative file names.  The service rejects `.` and
    # any other non-Normal path component before extraction.
    find . -type f -printf '%P\\0' | LC_ALL=C sort -z | \
        tar --null --verbatim-files-from --sort=name --mtime='@0' \
            --owner=0 --group=0 --numeric-owner --format=posix -T - -cf - | \
        zstd --quiet --no-progress --threads=1 -19 -o "$model_archive"
)

runtime_archive="$artifacts/vllm-openai-${RUNTIME_VERSION}-${RUNTIME_DIGEST}.oci.tar"
observed_runtime_digest=$(skopeo inspect --format '{{.Digest}}' "docker://${RUNTIME_IMAGE}")
[[ "$observed_runtime_digest" == "sha256:${RUNTIME_DIGEST}" ]] || {
    echo 'vLLM registry digest did not match the pinned runtime' >&2
    exit 1
}
skopeo copy --override-arch amd64 "docker://${RUNTIME_IMAGE}" \
    "oci-archive:${runtime_archive}:${RUNTIME_IMAGE}"

model_sha256=$(sha256sum "$model_archive" | awk '{print $1}')
model_size=$(stat --format='%s' "$model_archive")
runtime_sha256=$(sha256sum "$runtime_archive" | awk '{print $1}')
runtime_size=$(stat --format='%s' "$runtime_archive")
installed_size=$(find "$snapshot" -type f -printf '%s\n' | awk '{sum += $1} END {print sum}')
[[ "$installed_size" =~ ^[1-9][0-9]*$ ]] || {
    echo 'could not calculate installed snapshot size' >&2
    exit 1
}

publish_base=${publish_base%/}
model_name=$(basename -- "$model_archive")
runtime_name=$(basename -- "$runtime_archive")
model_url="${publish_base}/smolvlm2-500m/${MODEL_REVISION}/${model_name}"
runtime_url="${publish_base}/vllm/${RUNTIME_VERSION}/${runtime_name}"
cat > "$output/local-models.json" <<EOF
{
  "schema": 1,
  "production": true,
  "key_id": "tilo-local-model-release-2026-01",
  "recommendation": "${MODEL_ID}",
  "runtimes": [{
    "id": "vllm-cuda-${RUNTIME_VERSION}",
    "kind": "nvidia_cuda",
    "version": "${RUNTIME_VERSION}",
    "oci_image": "${RUNTIME_IMAGE}",
    "artifact": {
      "file": "artifacts/${runtime_name}",
      "url": "${runtime_url}",
      "sha256": "${runtime_sha256}",
      "size_bytes": ${runtime_size}
    },
    "base_image_digest": "${base_image_digest}",
    "architecture": "x86_64",
    "kernel_abi": null,
    "requires_reboot": false
  }],
  "models": [{
    "id": "${MODEL_ID}",
    "label": "SmolVLM2 500M Video Instruct",
    "description": "Local Apache-2.0 image captioning, labels, pet labels, and OCR through vLLM. This release does not provide face recognition or identity inference.",
    "revision": "${MODEL_REVISION}",
    "artifact": {
      "file": "artifacts/${model_name}",
      "url": "${model_url}",
      "sha256": "${model_sha256}",
      "size_bytes": ${model_size}
    },
    "artifact_format": "hf_snapshot_tar_zst",
    "license_id": "Apache-2.0",
    "license_url": "https://huggingface.co/${MODEL_REPOSITORY}/blob/${MODEL_REVISION}/README.md",
    "requires_acceptance": false,
    "installed_size_bytes": ${installed_size},
    "allowed_runtimes": ["nvidia_cuda"],
    "required_memory_mib": 16384,
    "required_accelerator_mib": 8192,
    "context_window": 8192,
    "vision": true,
    "face_recognition": false
  }]
}
EOF

printf '%s\n' "Staged ${model_archive} and ${runtime_archive}."
printf '%s\n' "Publish both files unchanged, verify their HTTPS receipts, then compile ${output}/local-models.json."
