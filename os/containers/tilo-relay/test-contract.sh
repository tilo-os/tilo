#!/usr/bin/env bash
# Static contract for the operator-facing composite home-peer deployment.
#
# Keep this cheap: t's image-relay check builds the image (tools/ci/publish/relay-build.sh,
# which runs this contract first), while this asserts the Compose artifact includes the
# complete filesystem replica and the image compiles nothing of its own.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
config="$root/relay.toml.example"
readme="$root/README.md"
containerfile="$root/Containerfile"
build="$root/../../../tools/ci/publish/relay-build.sh"
binaries="$root/../../../tools/ci/publish/server-binaries.sh"
compose="$root/compose.yaml"
entrypoint="$root/tilo-home-peer"

fail() { echo "tilo-relay container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }
forbid() { if grep -Fq -- "$2" "$1"; then fail "$(basename "$1") must not contain: $2"; fi; }

for path in "$compose" "$config" "$readme" "$build" "$binaries"; do
  [ -f "$path" ] || fail "missing $(basename "$path")"
done

# State must outlive the container. Enrollment creates only the scoped host
# identity here; the user's root private key never enters this deployment.
need "$compose" "./identity:/var/lib/tilo:Z"
need "$compose" "tilo-relay-state:/var/lib/tilo-relay:Z"
need "$compose" "network_mode: host"
need "$compose" "test -s /var/lib/tilo-relay/metrics.json"

# Parse the template with the standard-library TOML parser rather than trusting
# a visually plausible example that the daemon cannot load.
python3 - "$config" <<'PY'
import pathlib
import sys
import tomllib

cfg = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert cfg["identity_root"] == "/var/lib/tilo"
assert cfg["state_root"] == "/var/lib/tilo-relay"
assert cfg["iroh_listen"] == "0.0.0.0:4433"
assert cfg["trust_policy"] == {"kind": "allow_matching_user"}
PY

# Product boundary: this image promises verified full file residency.
need "$readme" "filesystem peer"
need "$readme" "selected workspace IDs"
need "$readme" "Pair home server"

# The image packages programs t built: the relay, its admin, its enroll program and the
# filesystem replica come from the staged context, and nothing in the image compiles.
need "$containerfile" "COPY bin/tilo-relay /usr/libexec/tilo/tilo-relay"
need "$containerfile" "COPY bin/tilo-relay-admin /usr/libexec/tilo/tilo-relay-admin"
need "$containerfile" "COPY bin/tilo-enroll /usr/libexec/tilo/tilo-enroll"
need "$containerfile" 'COPY bin/fs /usr/bin/fs'
forbid "$containerfile" "cargo build"
forbid "$containerfile" "--mount=type=cache"
forbid "$containerfile" "sccache"
forbid "$containerfile" "COPY . "
need "$containerfile" 'ENTRYPOINT ["/usr/bin/tilo-home-peer"]'
# The runtime keeps its OpenSSL-free, package-manager-free surface.
need "$containerfile" "microdnf install -y --setopt=reposdir=/repo --disablerepo='*' --enablerepo=frozen"
need "$containerfile" "microdnf remove -y --setopt=protected_packages= openssl-libs coreutils dnf5"

# The programs are one check's, thin LTO and read back off the ELF, from one feature resolution
# per image: the relay and the replica in one, as the image compiled them before.
need "$binaries" "packages=(-p tilo-relay -p tilo-fs --bins)"
need "$binaries" "bins=(tilo-relay tilo-relay-admin tilo-enroll fs)"
need "$binaries" "export CARGO_PROFILE_RELEASE_LTO=thin"
need "$binaries" 'bash os/scripts/verify-elf-hardening.sh "$out"'

# The build t runs (image-relay): this contract first; the base and the frozen RPM snapshot are
# passed by digest; the binaries are the check's output, staged with the entrypoint and the unit
# the image copies; nothing else enters the context, and a commit time is no input.
need "$build" "bash os/containers/tilo-relay/test-contract.sh"
need "$build" 'for bin in tilo-relay tilo-relay-admin tilo-enroll fs; do'
need "$build" '--build-arg "RPMS_REF=$rpms_ref"'
need "$build" 'cp os/containers/tilo-relay/tilo-home-peer "$context/os/containers/tilo-relay/"'
need "$build" 'cp os/usr/lib/systemd/system/tilo-relay.service "$context/os/usr/lib/systemd/system/"'
forbid "$build" '--build-arg "BASE_REF'
forbid "$build" 'SOURCE_DATE_EPOCH'
forbid "$build" "sccache"

need "$compose" "./config:/etc/tilo:Z"
need "$compose" "tilo-full-replica"
need "$entrypoint" "/usr/bin/fs replica"
need "$entrypoint" "/usr/libexec/tilo/tilo-relay"
need "$entrypoint" "/usr/libexec/tilo/tilo-enroll join"
need "$entrypoint" "/usr/libexec/tilo/tilo-enroll pair"
need "$entrypoint" "/var/lib/tilo-relay/decommissioned"
need "$entrypoint" "exec /usr/bin/busybox sleep infinity"
need "$entrypoint" "wait -n"

echo "tilo-relay container contract: PASS"
