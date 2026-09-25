#!/usr/bin/env bash
# Verify one CI-signed normal Lenovo OS image and export it with its Cosign
# attachments for a generic live graphical installer. No OS build occurs here.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

image="${1:?usage: prepare-lenovo-installer-target.sh IMAGE@sha256:DIGEST OUTPUT_DIR}"
output="${2:?missing output directory}"
[[ "$image" =~ ^code\.podesta\.ai/qos/tilo@sha256:[0-9a-f]{64}$ ]] \
  || die "Lenovo installer target must be the immutable CI child manifest"
[ ! -e "$output" ] && [ ! -L "$output" ] \
  || die "offline target output must not already exist"
for tool in cosign skopeo python3; do
  command -v "$tool" >/dev/null || die "$tool is required for the offline Lenovo target"
done
[ -s "$TILO_REPO/keys/cosign.pub" ] || die "official Cosign verification key is missing"

# Cosign uses Docker's auth directory while Podman/Skopeo use authfile. Both
# clients must see the same short-lived CI registry credential.
auth_dir=""
cleanup() { [ -z "$auth_dir" ] || rm -rf -- "$auth_dir"; }
trap cleanup EXIT
auth_args=()
if [ -n "${REGISTRY_AUTH_FILE:-}" ]; then
  [ -s "$REGISTRY_AUTH_FILE" ] || die "REGISTRY_AUTH_FILE is empty"
  auth_dir="$(mktemp -d)"
  chmod 0700 "$auth_dir"
  cp -- "$REGISTRY_AUTH_FILE" "$auth_dir/config.json"
  chmod 0600 "$auth_dir/config.json"
  export DOCKER_CONFIG="$auth_dir"
  auth_args=(--authfile "$REGISTRY_AUTH_FILE")
fi

manifest="$(mktemp)"
trap 'rm -f -- "$manifest"; cleanup' EXIT
skopeo inspect --raw "${auth_args[@]}" "docker://$image" > "$manifest"
expected="${image##*@}"
actual="sha256:$(sha256sum "$manifest" | awk '{print $1}')"
[ "$actual" = "$expected" ] || die "registry manifest differs from pinned CI digest"

# Reuse the Windows-direct verifier: same key, same SLSA predicate, same board
# constraints. The top-level and child are the same single manifest here.
source_revision="$(python3 - "$TILO_REPO" "$image" "$manifest" <<'PY'
import importlib.util
from pathlib import Path
import sys

repo, image, manifest = Path(sys.argv[1]), sys.argv[2], Path(sys.argv[3])
spec = importlib.util.spec_from_file_location(
    "windows_direct_provenance", repo / "os/dev/windows-direct-provenance.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
_, child, children, kind = module.signed_ci_child(image, manifest, manifest)
assert child == image.split("@", 1)[1] and kind == "lenovo-83ed" and not children
evidence = module.verify_ci_image(image, repo / "keys/cosign.pub", children, kind)
print(evidence["source_revision"])
PY
)"
[[ "$source_revision" =~ ^[0-9a-f]{40}$ ]] || die "SLSA source revision is invalid"
echo "verified CI Lenovo target $image from source $source_revision"

mkdir -p "$output/oci"
cosign save --dir "$output/oci" "$image"
python3 - "$image" "$source_revision" "$output/target.json" <<'PY'
import json
from pathlib import Path
import sys

image, revision, target = sys.argv[1:]
Path(target).write_text(json.dumps({
    "schema": "tilo.installer-offline-target.v1",
    "target_ref": image,
    "target_digest": image.split("@", 1)[1],
    "architecture": "aarch64",
    "hardware_profile": "lenovo-83ed",
    "update_origin": "code.sandbox.example/qos/tilo:lenovo-83ed-" + revision,
}, sort_keys=True, separators=(",", ":")) + "\n")
PY
python3 "$TILO_OS_DIR/dev/installer-offline-target.py" verify "$output"
echo "prepared offline CI target $image in $output"
