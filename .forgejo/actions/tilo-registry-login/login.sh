#!/usr/bin/env bash
# Log a container client in to registry.tilo.so with this job's Forgejo
# Actions ID token (see id-token.sh). Vendored from the infra repository's
# action of the same name, which documents the token service.
#
# Environment:
#   TILO_REGISTRY           registry host (default registry.tilo.so)
#   TILO_REGISTRY_AUDIENCE  ID token audience (default registry.tilo.so)
#   TILO_REGISTRY_CLIENT    docker, podman, buildah or skopeo (default docker)
set -euo pipefail

registry=${TILO_REGISTRY:-registry.tilo.so}
client=${TILO_REGISTRY_CLIENT:-docker}

case $client in
  docker | podman | buildah | skopeo) ;;
  *)
    echo "::error::unsupported client '$client'" >&2
    exit 1
    ;;
esac
"$(dirname "${BASH_SOURCE[0]}")/id-token.sh" \
  | "$client" login "$registry" --username oidc --password-stdin
