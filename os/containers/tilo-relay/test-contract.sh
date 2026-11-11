#!/usr/bin/env bash
# Static contract for the operator-facing composite home-peer deployment.
#
# Keep this cheap: t's image-relay check builds the image (tools/ci/publish/relay-build.sh,
# which runs this contract first), while this asserts the Compose artifact includes the
# complete filesystem replica.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
config="$root/relay.toml.example"
readme="$root/README.md"
containerfile="$root/Containerfile"
build="$root/../../../tools/ci/publish/relay-build.sh"
compose="$root/compose.yaml"
entrypoint="$root/tilo-home-peer"

fail() { echo "tilo-relay container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }
forbid() { if grep -Fq -- "$2" "$1"; then fail "$(basename "$1") must not contain: $2"; fi; }

for path in "$compose" "$config" "$readme" "$build"; do
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
# The image packages the programs t built: it compiles nothing itself, so a change costs that
# crate's compile and a layering run, and the toolchain is not in the image at all.
for program in tilo-relay tilo-relay-admin tilo-enroll fs; do
  need "$containerfile" "COPY bin/$program /usr/bin/$program"
done
for forbidden in cargo sccache rustup capnproto 'RUN --mount=type=cache'; do
  forbid "$containerfile" "$forbidden"
done
need "$containerfile" 'ENTRYPOINT ["/usr/bin/tilo-home-peer"]'
need "$compose" "./config:/etc/tilo:Z"
need "$compose" "tilo-full-replica"
need "$entrypoint" "/usr/bin/fs replica"
need "$entrypoint" "/usr/libexec/tilo/tilo-relay"
need "$entrypoint" "/usr/libexec/tilo/tilo-enroll join"
need "$entrypoint" "/usr/libexec/tilo/tilo-enroll pair"
need "$entrypoint" "/var/lib/tilo-relay/decommissioned"
need "$entrypoint" "exec /usr/bin/busybox sleep infinity"
need "$entrypoint" "wait -n"
# The build t runs (image-relay): this contract first; a commit time is no input (the
# Containerfile passes no SOURCE_DATE_EPOCH argument); the programs come from the cached
# relay-binaries check, not from a compile here; the frozen RPM snapshot is passed by digest; and
# the context is what the image copies, so no checkout metadata reaches it at all.
need "$build" "bash os/containers/tilo-relay/test-contract.sh"
if grep -Fq -- '--build-arg "SOURCE_DATE_EPOCH' "$build"; then
  fail "relay-build.sh passes a commit time into the image"
fi
if grep -Fq 'cargo build' "$build"; then
  fail "relay-build.sh still compiles the workspace"
fi
need "$build" 'for bin in tilo-relay tilo-relay-admin tilo-enroll fs; do'
need "$build" 'cp "$binaries/tilo-relay" "$binaries/tilo-relay-admin" "$binaries/tilo-enroll" "$binaries/fs"'
need "$build" '--build-arg "RPMS_REF=$rpms_ref"'
if grep -Fq -- '--build-context' "$build"; then
  fail "relay-build.sh takes a named context the package-only image does not use"
fi

echo "tilo-relay container contract: PASS"
