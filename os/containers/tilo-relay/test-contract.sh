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
dockerignore="$root/Containerfile.dockerignore"
build="$root/../../../tools/ci/publish/relay-build.sh"
compose="$root/compose.yaml"
entrypoint="$root/tilo-home-peer"

fail() { echo "tilo-relay container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }

for path in "$compose" "$config" "$readme" "$dockerignore" "$build"; do
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
need "$containerfile" "target/release/fs"
need "$containerfile" "rust cargo capnproto"
need "$containerfile" "CARGO_BUILD_JOBS=2"
need "$containerfile" "CARGO_PROFILE_RELEASE_LTO=thin"
need "$containerfile" "RUSTC_WRAPPER=/usr/local/bin/sccache"
need "$containerfile" "SCCACHE_SHA256_X86_64=67c4a96dd237c1f518f6b36083f270f9976d516f1e57fce891755ea782e50006"
need "$containerfile" "SCCACHE_SHA256_AARCH64=821a86343191aa1cbab74bd42f9e93c9a63bf85e4742945f40d3ae84193c1c77"
need "$containerfile" "SCCACHE_GHA_VERSION=tilo-relay-v1"
need "$containerfile" "--mount=type=secret,id=actions_cache_url"
need "$containerfile" "--mount=type=secret,id=actions_runtime_token"
need "$containerfile" "sccache --zero-stats"
need "$containerfile" "! sccache --start-server"
need "$containerfile" "export RUSTC_WRAPPER="
need "$containerfile" "sccache backend unavailable; compiling normally"
need "$containerfile" "required compiler cache configuration is unavailable"
need "$containerfile" "probe-sccache.py"
need "$containerfile" "test ! -e /context/.git/config"
need "$containerfile" "test ! -e /context/.forgejo"
need "$containerfile" "cargo build --locked --release -p tilo-relay -p tilo-fs --bins"
if grep -Fq 'sed -i' "$containerfile"; then
  fail "Containerfile mutates the locked workspace before compiling"
fi
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
# Containerfile's SOURCE_DATE_EPOCH defaults to 0 and no build argument overrides it); the pinned
# sccache release reaches the build only through the verified github-assets context; the base
# and the frozen RPM snapshot are passed by digest; the context is the ignore file's filtered copy.
need "$build" "bash os/containers/tilo-relay/test-contract.sh"
need "$containerfile" "ARG SOURCE_DATE_EPOCH=0"
if grep -Fq -- '--build-arg "SOURCE_DATE_EPOCH' "$build" || grep -Fq -- '--build-arg SOURCE_DATE_EPOCH' "$build"; then
  fail "relay-build.sh passes a commit time into the image"
fi
if grep -Fq 'SCCACHE_AWS' "$build"; then
  fail "relay-build.sh binds the static compiler-cache key"
fi
need "$build" 'assets=$(pinned_assets sccache-v0.17.0-x86_64-unknown-linux-musl.tar.gz)'
need "$build" '--build-context "github-assets=$assets"'
need "$build" '--build-arg "BASE_REF=$BASE_REF"'
need "$build" '--build-arg "RPMS_REF=$rpms_ref"'
need "$build" 'stage_context os/containers/tilo-relay/Containerfile.dockerignore "$context"'
need "$build" '--ignorefile os/containers/tilo-relay/Containerfile.dockerignore'

for excluded in .git .forgejo target os/output; do
  grep -qxF "$excluded" "$dockerignore" || fail "Docker build context includes $excluded"
done

echo "tilo-relay container contract: PASS"
