#!/usr/bin/env bash
# Prove a brokered GitHub App installation token can see exactly one repository.
set -euo pipefail

repository="${1:?usage: assert-github-installation-token.sh OWNER/REPOSITORY}"
: "${GITHUB_INSTALLATION_TOKEN:?missing GitHub App installation token}"
api="${TILO_RELEASE_API_URL:-https://api.github.com}"
[[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] \
  || { printf 'GitHub repository must be OWNER/REPOSITORY\n' >&2; exit 2; }
[[ "$api" =~ ^https://[^/]+$ || "$api" =~ ^http://127\.0\.0\.1:[0-9]+$ ]] \
  || { printf 'GitHub API must be an HTTPS origin\n' >&2; exit 2; }

response="$(mktemp)"
trap 'rm -f "$response"' EXIT
curl --fail --silent --show-error \
  --retry 5 --retry-all-errors --retry-max-time 120 \
  --connect-timeout 20 --max-time 120 \
  -H "Authorization: Bearer $GITHUB_INSTALLATION_TOKEN" \
  -H 'Accept: application/vnd.github+json' \
  -H 'X-GitHub-Api-Version: 2022-11-28' \
  "$api/installation/repositories?per_page=100" >"$response"
python3 - "$response" "$repository" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as source:
    payload = json.load(source)
repositories = payload.get("repositories")
total = payload.get("total_count")
if not isinstance(repositories, list) or not isinstance(total, int):
    raise SystemExit("GitHub did not return installation-token repository scope")
names = [entry.get("full_name") for entry in repositories]
if total != 1 or names != [sys.argv[2]]:
    raise SystemExit(
        "GitHub App installation token must be narrowed to exactly " + sys.argv[2]
    )
PY
