#!/usr/bin/env bash
# Revoke the current GitHub App installation token after its one promotion step.
set -euo pipefail
: "${GITHUB_INSTALLATION_TOKEN:?missing GitHub App installation token}"
api="${TILO_RELEASE_API_URL:-https://api.github.com}"
curl --fail --silent --show-error -X DELETE \
  --connect-timeout 20 --max-time 60 \
  -H "Authorization: Bearer $GITHUB_INSTALLATION_TOKEN" \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  "$api/installation/token"
