#!/usr/bin/env bash
# Static contract for the operator-facing composite home-peer deployment.
#
# Keep this cheap: image construction is covered by release-relay.yml, while this
# asserts the Compose artifact includes the complete filesystem replica.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
config="$root/relay.toml.example"
readme="$root/README.md"
containerfile="$root/Containerfile"
dockerignore="$root/Containerfile.dockerignore"
workflow="$root/../../../.forgejo/workflows/release-relay.yml"
compose="$root/compose.yaml"
entrypoint="$root/tilo-home-peer"

bash "$root/test-base-retry.sh"

fail() { echo "tilo-relay container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }

for path in "$compose" "$config" "$readme" "$dockerignore" "$workflow"; do
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
need "$entrypoint" "/usr/bin/tilo-relay"
need "$entrypoint" "/usr/bin/tilo-enroll join"
need "$entrypoint" "/usr/bin/tilo-enroll pair"
need "$entrypoint" "/var/lib/tilo-relay/decommissioned"
need "$entrypoint" "exec /usr/bin/busybox sleep infinity"
need "$entrypoint" "wait -n"
need "$workflow" "persist-credentials: false"
need "$workflow" "SOURCE_DATE_EPOCH=0"
need "$workflow" "actions_cache_url=ACTIONS_CACHE_URL"
need "$workflow" "actions_runtime_token=ACTIONS_RUNTIME_TOKEN"
if grep -Fq 'SCCACHE_AWS' "$workflow"; then
  fail "release-relay binds the static compiler-cache key"
fi
need "$workflow" "Bind the runner compiler-cache configuration"
need "$workflow" "TILO_REQUIRE_COMPILER_CACHE="
need "$workflow" "TILO_COMPILER_CACHE_CONFIGURED="
need "$workflow" "TILO_CACHE_PROBE_NONCE="
need "$workflow" 'github-assets=os/external-assets/cache'
need "$workflow" 'uses: ./.forgejo/actions/cache-github-assets'
need "$workflow" 'assets: sccache-v0.17.0-x86_64-unknown-linux-musl.tar.gz'
need "$workflow" "driver-opts: network=host"
# Publications never build on a builder whose state another job left behind.
need "$workflow" "persistent: \${{ !(startsWith(github.ref, 'refs/tags/v') || github.event_name == 'workflow_dispatch') }}"
need "$workflow" 'cache-to: type=gha,scope=relay,mode=max,ignore-error=true'

for excluded in .git .forgejo target os/output; do
  grep -qxF "$excluded" "$dockerignore" || fail "Docker build context includes $excluded"
done

echo "tilo-relay container contract: PASS"
