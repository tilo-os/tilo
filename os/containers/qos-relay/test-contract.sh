#!/usr/bin/env bash
# Static contract for the operator-facing composite home-peer deployment.
#
# Keep this cheap: image construction is covered by qos-relay.yaml, while this
# asserts the Compose artifact includes the complete filesystem replica.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
compose="$root/compose.yaml"
config="$root/relay.toml.example"
readme="$root/README.md"
containerfile="$root/Containerfile"
compose="$root/compose.yaml"
entrypoint="$root/qos-home-peer"

fail() { echo "qos-relay container contract: FAIL: $*" >&2; exit 1; }
need() { grep -Fq -- "$2" "$1" || fail "$(basename "$1") lacks: $2"; }

for path in "$compose" "$config" "$readme"; do
  [ -f "$path" ] || fail "missing $(basename "$path")"
done

# State must outlive the container. Enrollment creates only the scoped host
# identity here; the user's root private key never enters this deployment.
need "$compose" "./identity:/var/lib/qos:Z"
need "$compose" "qos-relay-state:/var/lib/qos-relay:Z"
need "$compose" "network_mode: host"
need "$compose" "test -s /var/lib/qos-relay/metrics.json"

# Parse the template with the standard-library TOML parser rather than trusting
# a visually plausible example that the daemon cannot load.
python3 - "$config" <<'PY'
import pathlib
import sys
import tomllib

cfg = tomllib.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
assert cfg["identity_root"] == "/var/lib/qos"
assert cfg["state_root"] == "/var/lib/qos-relay"
assert cfg["iroh_listen"] == "0.0.0.0:4433"
assert cfg["trust_policy"] == {"kind": "allow_matching_user"}
PY

# Product boundary: this image promises verified full file residency.
need "$readme" "filesystem peer"
need "$readme" "selected workspace IDs"
need "$readme" "Pair home server"
need "$containerfile" "target/release/fs"
need "$containerfile" "rust cargo sccache capnproto"
need "$containerfile" "CARGO_BUILD_JOBS=2"
need "$containerfile" "CARGO_PROFILE_RELEASE_LTO=thin"
need "$containerfile" "cargo build --release -p qos-relay --bins"
need "$containerfile" "cargo build --release -p fs --bin fs"
need "$containerfile" 'ENTRYPOINT ["/usr/bin/qos-home-peer"]'
need "$compose" "./config:/etc/qos:Z"
need "$compose" "qos-full-replica"
need "$entrypoint" "/usr/bin/fs replica"
need "$entrypoint" "/usr/bin/qos-relay"
need "$entrypoint" "/usr/bin/qos-enroll join"
need "$entrypoint" "/usr/bin/qos-enroll pair"
need "$entrypoint" "/var/lib/qos-relay/decommissioned"
need "$entrypoint" "exec sleep infinity"
need "$entrypoint" "wait -n"

echo "qos-relay container contract: PASS"
