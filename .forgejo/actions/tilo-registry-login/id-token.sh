#!/usr/bin/env bash
# Print this job's Forgejo Actions ID token for the registry.tilo.so token
# service. The job needs `enable-openid-connect: true`; GitHub's
# `permissions: id-token: write` does not make Forgejo inject the token.
#
# The token is the registry password for user name `oidc`; the token service
# verifies it against the forge's OIDC issuer and grants push below tilo/ to
# allow-listed source repositories. Forgejo ID tokens live one hour, so fetch
# one right before logging in and again before a push that starts later.
#
# Clients that need their own login invocation (sudo, --authfile) pipe it:
#   id-token.sh | sudo -n podman login "$TILO_REGISTRY" --username oidc --password-stdin
#
# Environment:
#   TILO_REGISTRY_AUDIENCE  ID token audience (default registry.tilo.so)
set -euo pipefail

audience=${TILO_REGISTRY_AUDIENCE:-registry.tilo.so}

if [[ -z ${ACTIONS_ID_TOKEN_REQUEST_URL:-} || -z ${ACTIONS_ID_TOKEN_REQUEST_TOKEN:-} ]]; then
  echo "::error::no ID token endpoint; set 'enable-openid-connect: true' on the job" >&2
  exit 1
fi

separator='?'
[[ $ACTIONS_ID_TOKEN_REQUEST_URL == *\?* ]] && separator='&'
response=$(curl --fail --silent --show-error --retry 3 \
  --header "Authorization: bearer $ACTIONS_ID_TOKEN_REQUEST_TOKEN" \
  "${ACTIONS_ID_TOKEN_REQUEST_URL}${separator}audience=${audience}")
if command -v jq >/dev/null; then
  jwt=$(jq -er .value <<<"$response")
else
  jwt=$(python3 -c 'import json, sys; print(json.load(sys.stdin)["value"])' <<<"$response")
fi
unset response
echo "::add-mask::$jwt" >&2
printf '%s' "$jwt"
