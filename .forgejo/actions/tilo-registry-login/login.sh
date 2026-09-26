#!/usr/bin/env bash
# Configure this job's container clients to authenticate to registry.tilo.so
# with a fresh Forgejo Actions ID token on every request.
#
# The token service accepts the ID token as the password of user `oidc` and
# grants push below tilo/ to allow-listed source repositories. An ID token
# lives one hour and builds routinely outlast that, so instead of a one-shot
# `login` this installs a Docker credential helper (docker-credential-tilo-
# registry) and names it for the registry in the Docker config. docker,
# buildx, cosign, podman and skopeo all consult it whenever they need
# credentials, and it fetches a new token each time.
#
# Environment:
#   TILO_REGISTRY           registry host (default registry.tilo.so)
#   TILO_REGISTRY_AUDIENCE  ID token audience (default registry.tilo.so)
set -euo pipefail

registry=${TILO_REGISTRY:-registry.tilo.so}
audience=${TILO_REGISTRY_AUDIENCE:-registry.tilo.so}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
for value in "$registry" "$audience"; do
  [[ $value =~ ^[a-z0-9.-]+(:[0-9]+)?$ ]] || {
    echo "::error::invalid registry or audience: $value" >&2
    exit 1
  }
done

# Fail here, not at the first push, when the job has no ID token.
TILO_REGISTRY_AUDIENCE=$audience "$here/id-token.sh" >/dev/null

# The helper runs from its own directory: clients find it on PATH in every
# later step, independent of the checkout.
bin="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/tilo-registry-credential"
mkdir -p "$bin"
install -m 0755 "$here/id-token.sh" "$here/docker-credential-tilo-registry" "$bin/"
printf '%s\n' "$audience" > "$bin/audience"
if [[ -n ${GITHUB_PATH:-} ]]; then
  printf '%s\n' "$bin" >> "$GITHUB_PATH"
fi

config_dir=${DOCKER_CONFIG:-$HOME/.docker}
config="$config_dir/config.json"
mkdir -p "$config_dir"
[[ -s $config ]] || printf '{}\n' > "$config"
# Name the helper for this registry and drop any static credential for it.
if command -v jq >/dev/null; then
  updated=$(jq --arg registry "$registry" \
    '.credHelpers[$registry] = "tilo-registry" | del(.auths[$registry])' "$config")
else
  updated=$(python3 -c '
import json, sys
config = json.load(open(sys.argv[1]))
config.setdefault("credHelpers", {})[sys.argv[2]] = "tilo-registry"
config.get("auths", {}).pop(sys.argv[2], None)
print(json.dumps(config, indent=2))' "$config" "$registry")
fi
printf '%s\n' "$updated" > "$config"
chmod 0600 "$config"
echo "registry.tilo.so credentials: fresh ID token per request via $bin/docker-credential-tilo-registry"
