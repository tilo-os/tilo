#!/usr/bin/env bash
# Static contract for the operator-facing server container deployment.
#
# Keep this cheap: t's image-server check builds the image (tools/ci/publish/server-build.sh,
# which runs this contract first), while this asserts the Compose artifact keeps its state and
# the image compiles nothing of its own.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
config="$root/server.toml.example"
readme="$root/README.md"
containerfile="$root/Containerfile"
build="$root/../../../tools/ci/publish/server-build.sh"
binaries="$root/../../../tools/ci/publish/server-binaries.sh"
entrypoint="$root/tilo-home-peer"

fail() { echo "tilo-server container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }
forbid() { if grep -Fq -- "$2" "$1"; then fail "$(basename "$1") must not contain: $2"; fi; }

for path in "$compose" "$config" "$readme" "$build" "$binaries"; do
  [ -f "$path" ] || fail "missing $(basename "$path")"
done

# State must outlive the container. A ticket's login leaves only the scoped host
# identity here; the user's root private key never enters this deployment.
need "$compose" "./identity:/var/lib/tilo:Z"
need "$compose" "tilo-server-state:/var/lib/tilo-server:Z"
need "$compose" "network_mode: host"
need "$compose" "test -S /var/lib/tilo-server/native.sock"

# Parse the template with the standard-library TOML parser rather than trusting
# a visually plausible example that the daemon cannot load.
python3 - "$config" <<'PY'
import pathlib
import sys
import tomllib

cfg = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert cfg["iroh_listen"] == "0.0.0.0:4433"
assert cfg["trust_policy"] == {"kind": "allow_matching_user"}
PY

need "$readme" "tilo device add --server"
need "$readme" "TILO_TICKET"

# The image packages programs t built: the device's daemon, which is the server, its admin, and
# the command beside them, from the staged context; nothing in the image compiles.
forbid "$containerfile" "COPY bin/tilo-server /"
need "$containerfile" "COPY bin/tilo-server-admin /usr/libexec/tilo/tilo-server-admin"
need "$containerfile" "COPY bin/tilo /usr/bin/tilo"
need "$containerfile" "COPY bin/tilo-runtime /usr/bin/tilo-runtime"
forbid "$containerfile" 'COPY bin/fs'
forbid "$containerfile" "cargo build"
forbid "$containerfile" "--mount=type=cache"
forbid "$containerfile" "sccache"
forbid "$containerfile" "COPY . "
need "$containerfile" 'ENTRYPOINT ["/usr/bin/tilo-home-peer"]'
# The runtime keeps its OpenSSL-free, package-manager-free surface.
need "$containerfile" "microdnf install -y --setopt=reposdir=/repo --disablerepo='*' --enablerepo=frozen"
need "$containerfile" "microdnf remove -y --setopt=protected_packages= openssl-libs coreutils dnf5"

# The programs are one check's, thin LTO and read back off the ELF, from one feature resolution
# per image. Thin LTO is
# the default; only a development image's build asks for none, and the server image's does not.
need "$binaries" "packages+=(-p tilo-server --bin tilo-server-admin"
need "$binaries" "bins+=(tilo-server-admin tilo tilo-runtime)"
need "$binaries" ': "${TILO_SERVER_LTO:=thin}"'
need "$binaries" "thin) export CARGO_PROFILE_RELEASE_LTO=thin ;;"
need "$binaries" 'bash os/scripts/verify-elf-hardening.sh "$out"'

# The build t runs (image-server): this contract first; the base and the frozen RPM snapshot are
# passed by digest; the binaries are the check's output, staged with the entrypoint and the unit
# the image copies; nothing else enters the context, and a commit time is no input.
need "$build" "bash os/containers/tilo-server/test-contract.sh"
need "$build" 'for bin in tilo-server-admin tilo tilo-runtime; do'
need "$build" '--build-arg "RPMS_REF=$rpms_ref"'
need "$build" 'cp os/containers/tilo-server/tilo-home-peer "$context/os/containers/tilo-server/"'
need "$build" 'cp os/usr/lib/systemd/system/tilo-server.service "$context/os/usr/lib/systemd/system/"'
forbid "$build" '--build-arg "BASE_REF'
forbid "$build" 'SOURCE_DATE_EPOCH'
forbid "$build" "sccache"

need "$compose" "./config:/etc/tilo:Z"
need "$compose" "TILO_TICKET"
forbid "$compose" "full-replica"
forbid "$entrypoint" "/usr/bin/fs"
need "$entrypoint" "exec /usr/bin/tilo-runtime"
need "$entrypoint" "TILO_SERVER_CONFIG=/etc/tilo/server.toml"
need "$entrypoint" "tilo.ticket"
need "$entrypoint" "/var/lib/tilo-server/decommissioned"
need "$entrypoint" "exec /usr/bin/busybox sleep infinity"

echo "tilo-server container contract: PASS"
