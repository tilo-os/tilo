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

# State must outlive the container; identity is intentionally mounted
# read-only, never created by this deployment.
need "$compose" "./identity:/var/lib/qos:ro,Z"
need "$compose" "qos-relay-state:/var/lib/qos-relay:Z"
need "$compose" "test -s /var/lib/qos-relay/metrics.json"
need "$compose" '"${QOS_RELAY_PORT:-4433}:4433/udp"'

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
need "$readme" "workspaces[]"
need "$readme" "does not enroll devices or mint certificates"
need "$containerfile" "target/release/fs"
need "$containerfile" 'ENTRYPOINT ["/usr/bin/qos-home-peer"]'
need "$compose" "./home-peer.toml:/etc/qos/home-peer.toml"
need "$compose" "qos-full-replica"
need "$entrypoint" "/usr/bin/fs replica"
need "$entrypoint" "/usr/bin/qos-relay"
need "$entrypoint" "wait -n"

echo "qos-relay container contract: PASS"
