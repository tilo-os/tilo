#!/usr/bin/env bash
# Verify one CI-signed normal ARM64 OS image and export it with its Cosign
# attachments for a generic live graphical installer. No OS build occurs here.
set -euo pipefail

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

profile="${1:?usage: prepare-ci-installer-target.sh generic|lenovo-83ed CHILD@sha256:DIGEST OUTPUT_DIR [GENERIC_INDEX_REF]}"
image="${2:?missing signed ARM64 child}"
output="${3:?missing output directory}"
index_image="${4:-}"
case "$profile" in
  generic) [[ "$index_image" =~ ^registry\.tilo\.so/tilo/tilo@sha256:[0-9a-f]{64}$ ]] \
    || die "generic target requires a signed generic release index" ;;
  lenovo-83ed) [ -z "$index_image" ] || die "Lenovo target is a single signed child" ;;
  *) die "unsupported installer target profile" ;;
esac
# A board child carries private firmware and stays on the forge's private
# registry; the generic child is public.
case "$profile" in
  generic) repository='registry\.tilo\.so/tilo/tilo' ;;
  lenovo-83ed) repository='code\.tilo\.so/tilo/tilo' ;;
esac
[[ "$image" =~ ^${repository}@sha256:[0-9a-f]{64}$ ]] \
  || die "installer target must be the immutable CI ARM64 child manifest"
[ ! -e "$output" ] && [ ! -L "$output" ] \
  || die "offline target output must not already exist"
for tool in cosign skopeo python3; do
  command -v "$tool" >/dev/null || die "$tool is required for the offline CI target"
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
index_manifest="$(mktemp)"
trap 'rm -f -- "$manifest" "$index_manifest"; cleanup' EXIT
skopeo inspect --raw "${auth_args[@]}" "docker://$image" > "$manifest"
expected="${image##*@}"
actual="sha256:$(sha256sum "$manifest" | awk '{print $1}')"
[ "$actual" = "$expected" ] || die "registry manifest differs from pinned CI digest"
if [ "$profile" = generic ]; then
  skopeo inspect --raw "${auth_args[@]}" "docker://$index_image" > "$index_manifest"
fi

# Reuse the Windows-direct verifier for the signed board child and published
# generic index. Generic child signatures receive an additional, independent
# digest/source/generation check before their offline Cosign bundle is saved.
verified="$(python3 - "$TILO_REPO" "$profile" "$image" "$manifest" "$index_image" "$index_manifest" <<'PY'
import base64
import binascii
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys

repo, profile, image, manifest, index_image, index_manifest = (
    Path(sys.argv[1]), sys.argv[2], sys.argv[3], Path(sys.argv[4]), sys.argv[5], Path(sys.argv[6]))
spec = importlib.util.spec_from_file_location(
    "windows_direct_provenance", repo / "os/dev/windows-direct-provenance.py")
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
key = repo / "keys/cosign.pub"
if profile == "lenovo-83ed":
    _, child, children, kind = module.signed_ci_child(image, manifest, manifest)
    module.require(child == image.split("@", 1)[1] and kind == "lenovo-83ed" and not children,
                   "Lenovo target differs from signed child")
    evidence = module.verify_ci_image(image, key, children, kind)
else:
    _, child, children, kind = module.signed_ci_child(index_image, index_manifest, manifest)
    module.require(kind == "generic-index" and child == image.split("@", 1)[1],
                   "generic target is not the signed index ARM64 child")
    evidence = module.verify_ci_image(index_image, key, children, kind)
    signatures = subprocess.check_output(["cosign", "verify", "--key", str(key), image], text=True)
    module.require(any(row.get("critical", {}).get("image", {}).get("docker-manifest-digest") == child
                       for row in module.wrappers(signatures)), "generic child signature differs")
    attestations = subprocess.check_output(["cosign", "verify-attestation", "--key", str(key),
                                            "--type", "slsaprovenance1", image], text=True)
    matches = []
    for row in module.wrappers(attestations):
        try:
            statement = json.loads(base64.b64decode(row["payload"], validate=True))
            predicate = statement["predicate"]
            definition = predicate["buildDefinition"]
            details = predicate["runDetails"]
            external = definition["externalParameters"]
            dependencies = definition["resolvedDependencies"]
            valid = (statement.get("predicateType") == "https://slsa.dev/provenance/v1"
                     and any(item.get("digest", {}).get("sha256") == child.removeprefix("sha256:")
                             for item in statement["subject"])
                     and definition.get("buildType") == module.BUILD_TYPE
                     and definition["internalParameters"].get("expectedReleaseDigest") == child
                     and details["builder"].get("id") == module.BUILDER_ID
                     and external.get("generation") == evidence["generation"]
                     and "hardwareProfile" not in external and "children" not in external
                     and any(item.get("uri") == "git+ssh://code.tilo.so/tilo/tilo@"
                             + evidence["source_revision"] for item in dependencies)
                     and re.fullmatch(r"[0-9]+", details["metadata"]["invocationId"]) is not None
                     and any(item.get("name") == "independent-rebuild"
                             and item.get("uri") == "urn:tilo:verification:manifest-digest-equal"
                             for item in details["byproducts"]))
            if valid:
                matches.append(statement)
        except (KeyError, TypeError, ValueError, IndexError, AttributeError, binascii.Error):
            continue
    module.require(len(matches) == 1, "generic child SLSA does not bind signed index, source, and generation")
print(json.dumps({"source_revision": evidence["source_revision"],
                  "generation": evidence["generation"]}))
PY
)"
source_revision="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["source_revision"])' <<<"$verified")"
generation="$(python3 -c 'import json,sys; print(json.loads(sys.stdin.read())["generation"])' <<<"$verified")"
[[ "$source_revision" =~ ^[0-9a-f]{40}$ ]] || die "SLSA source revision is invalid"
if [ -n "${TILO_EXPECTED_OS_SOURCE_REVISION:-}" ]; then
  [[ "$TILO_EXPECTED_OS_SOURCE_REVISION" =~ ^[0-9a-f]{40}$ ]] \
    || die "expected OS source revision is invalid"
  [ "$source_revision" = "$TILO_EXPECTED_OS_SOURCE_REVISION" ] \
    || die "signed $profile target source differs from pinned OS source"
fi
[[ "$generation" =~ ^[1-9][0-9]*$ ]] || die "SLSA generation is invalid"
[ "$profile" != lenovo-83ed ] || [ "$generation" = 1 ] || die "Lenovo board must be generation 1"
echo "verified CI $profile target $image from source $source_revision generation $generation"

mkdir -p "$output/oci"
cosign save --dir "$output/oci" "$image"
python3 - "$profile" "$image" "$source_revision" "$generation" "$output/target.json" <<'PY'
import json
from pathlib import Path
import sys

profile, image, revision, generation, target = sys.argv[1:]
origin = ("code.tilo.so/tilo/tilo:lenovo-83ed-" + revision if profile == "lenovo-83ed"
          else "registry.tilo.so/tilo/tilo:gen" + generation)
Path(target).write_text(json.dumps({
    "schema": "tilo.installer-offline-target.v1",
    "target_ref": image,
    "target_digest": image.split("@", 1)[1],
    "architecture": "aarch64",
    "hardware_profile": profile,
    "update_origin": origin,
}, sort_keys=True, separators=(",", ":")) + "\n")
PY
python3 "$TILO_OS_DIR/dev/installer-offline-target.py" verify "$output"
echo "prepared offline CI target $image in $output"
