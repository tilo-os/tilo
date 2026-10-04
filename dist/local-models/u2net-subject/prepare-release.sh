#!/usr/bin/env bash
# Stage the U²-Net subject selection model as an in-process local-model
# release: one verified ONNX file plus the licence it ships under, and the
# catalog source entry naming them. This script does not sign or publish.
set -euo pipefail

readonly MODEL_ID='u2net-subject'
# The revision is the SHA-256 of the ONNX graph itself. The upstream asset
# below lives under a release tag, which can move; the digest cannot, and the
# published copy carries it in its path.
readonly MODEL_REVISION='8d10d2f3bb75ae3b6d527c77944fc5e7dcd94b29809d47a739a7a728a912b491'
readonly MODEL_SIZE=175997641
# U²-Net's general salient-object model, converted to ONNX and published by
# rembg. Only its digest is trusted.
readonly MODEL_UPSTREAM='https://github.com/danielgatis/rembg/releases/download/v0.0.0/u2net.onnx'
# The Apache-2.0 licence of the upstream model at an exact commit.
readonly LICENSE_COMMIT='ac7e1c817ecab7c7dff5ce6b1abba61cd213ff29'
readonly LICENSE_UPSTREAM="https://raw.githubusercontent.com/xuebinqin/U-2-Net/${LICENSE_COMMIT}/LICENSE"

usage() {
    cat <<'EOF'
Usage: prepare-release.sh --output DIR --publish-base-url HTTPS_URL

Fetches u2net.onnx and the upstream LICENSE, checks both against the pinned
model-files.sha256, stages them under artifacts/, and writes
u2net-subject.model.json: one catalog source model entry to add to the
release's local-models.json "models" array. The publish base must be the
permanent HTTPS location where both files will be uploaded, unchanged, at
<base>/u2net-subject/<revision>/.
EOF
}

output=''
publish_base=''
while (($#)); do
    case "$1" in
        --output) output=${2:?missing output}; shift 2 ;;
        --publish-base-url) publish_base=${2:?missing publish URL}; shift 2 ;;
        --help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
done

[[ -n "$output" && -n "$publish_base" ]] || {
    usage >&2
    exit 2
}
[[ "$publish_base" =~ ^https://[^?#]+$ ]] || {
    echo 'publish base URL must be HTTPS and contain no query or fragment' >&2
    exit 2
}
for command in curl sha256sum awk stat; do
    command -v "$command" >/dev/null || {
        echo "required command is unavailable: $command" >&2
        exit 127
    }
done

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
output=$(mkdir -p "$output" && CDPATH= cd -- "$output" && pwd)
artifacts="$output/artifacts"
[[ ! -e "$artifacts" ]] || {
    echo 'output must not already contain an artifacts directory' >&2
    exit 2
}
mkdir -p "$artifacts"

fetch_pinned() {
    local url=$1 file=$2 expected
    expected=$(awk -v name="$file" '$2 == name {print $1}' "$script_dir/model-files.sha256")
    [[ "$expected" =~ ^[0-9a-f]{64}$ ]] || {
        echo "no pinned digest for $file" >&2
        exit 1
    }
    curl --fail --location --proto '=https' --tlsv1.2 --retry 3 \
        --output "$artifacts/$file" "$url"
    [[ "$(sha256sum "$artifacts/$file" | awk '{print $1}')" == "$expected" ]] || {
        echo "upstream digest mismatch: $file" >&2
        exit 1
    }
}

fetch_pinned "$MODEL_UPSTREAM" u2net.onnx
fetch_pinned "$LICENSE_UPSTREAM" LICENSE
[[ "$(sha256sum "$artifacts/u2net.onnx" | awk '{print $1}')" == "$MODEL_REVISION" ]] || {
    echo 'the model revision must be the digest of the ONNX file' >&2
    exit 1
}
[[ "$(stat --format='%s' "$artifacts/u2net.onnx")" == "$MODEL_SIZE" ]] || {
    echo 'u2net.onnx does not have its pinned size' >&2
    exit 1
}

publish_base=${publish_base%/}
release_url="${publish_base}/${MODEL_ID}/${MODEL_REVISION}"
cat > "$output/${MODEL_ID}.model.json" <<EOF
{
  "id": "${MODEL_ID}",
  "label": "Subject selection (U²-Net)",
  "description": "Finds the main subject of a photo so apps can select or cut it out in one step. U²-Net runs on this device's processor inside the apps that use it; images never leave this device.",
  "revision": "${MODEL_REVISION}",
  "artifact": {
    "file": "${artifacts}/u2net.onnx",
    "url": "${release_url}/u2net.onnx",
    "sha256": "${MODEL_REVISION}",
    "size_bytes": ${MODEL_SIZE}
  },
  "artifact_format": "onnx_file",
  "license_id": "Apache-2.0",
  "license_url": "${release_url}/LICENSE",
  "requires_acceptance": true,
  "installed_size_bytes": ${MODEL_SIZE},
  "allowed_runtimes": [],
  "required_memory_mib": 1024,
  "required_accelerator_mib": 0,
  "context_window": 0,
  "vision": false,
  "face_recognition": false,
  "segmentation": true
}
EOF

printf '%s\n' "Staged ${artifacts}/u2net.onnx and ${artifacts}/LICENSE."
printf '%s\n' "Upload both unchanged to ${release_url}/, verify their HTTPS receipts, then add ${output}/${MODEL_ID}.model.json to the release's local-models.json models."
