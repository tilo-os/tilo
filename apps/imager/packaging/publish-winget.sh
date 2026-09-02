#!/usr/bin/env bash
# Submit the generated qOS.Imager manifest to Microsoft's public WinGet index.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
: "${WINGET_GITHUB_TOKEN:?missing protected GitHub token}"
: "${WINGET_GITHUB_USER:?missing GitHub account owning the submission fork}"
[[ "$WINGET_GITHUB_USER" =~ ^[A-Za-z0-9-]+$ ]] \
  || { printf 'WINGET_GITHUB_USER is not a valid GitHub login\n' >&2; exit 2; }

assets="${1:-release-assets}"
version="${GITHUB_REF_NAME#imager-v}"
[ "$version" != "$GITHUB_REF_NAME" ] || { printf 'expected imager-v* tag\n' >&2; exit 2; }
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || { printf 'release tag must be imager-vMAJOR.MINOR.PATCH\n' >&2; exit 2; }
manifest_source="$(find "$assets" -type d -name winget -print -quit)"
[ -n "$manifest_source" ] || { printf 'WinGet manifests were not downloaded\n' >&2; exit 1; }
[ "$(find "$manifest_source" -maxdepth 1 -name '*.yaml' | wc -l)" -eq 3 ] \
  || { printf 'expected exactly three WinGet manifest files\n' >&2; exit 1; }

api=https://api.github.com
auth="Authorization: Bearer ${WINGET_GITHUB_TOKEN}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# GitHub is the public WinGet index's control plane, so these calls cannot use
# an immutable artifact cache. Keep the exposure authenticated and bounded;
# --fail-with-body makes retryable 408/429/5xx responses fail closed while
# preserving GitHub's response for diagnostics.
api_call() {
  local output="$1"
  shift
  API_CODE=''
  API_RC=0
  if API_CODE="$(curl --fail-with-body --silent --show-error --location \
    --retry 6 --retry-all-errors --retry-max-time 180 --connect-timeout 20 --max-time 45 \
    -H "$auth" -H 'Accept: application/vnd.github+json' \
    -H 'X-GitHub-Api-Version: 2022-11-28' \
    --output "$output" --write-out '%{http_code}' "$@")"; then
    API_RC=0
  else
    API_RC=$?
  fi
}

repo_response="$work/repo.json"
api_call "$repo_response" "$api/repos/${WINGET_GITHUB_USER}/winget-pkgs"
case "$API_CODE" in
  200) ;;
  404)
    fork_response="$work/fork.json"
    api_call "$fork_response" -X POST -H 'Content-Type: application/json' \
      --data '{}' "$api/repos/microsoft/winget-pkgs/forks"
    case "$API_CODE" in
      200|201|202) ;;
      422)
        # A timed-out successful POST may be retried after GitHub has created
        # the fork. Prove that exact end state instead of treating every 422 as
        # success.
        api_call "$repo_response" "$api/repos/${WINGET_GITHUB_USER}/winget-pkgs"
        [ "$API_CODE" = 200 ] || { cat "$fork_response" >&2; exit 1; }
        ;;
      *) cat "$fork_response" >&2; exit 1 ;;
    esac
    ;;
  *)
    cat "$repo_response" >&2
    [ "$API_RC" -ne 0 ] || API_RC=1
    exit "$API_RC"
    ;;
esac

askpass="$work/askpass.sh"
printf '%s\n' '#!/bin/sh' \
  'case "$1" in *Username*) printf x-access-token ;; *) printf "%s" "$WINGET_GITHUB_TOKEN" ;; esac' \
  > "$askpass"
chmod 0700 "$askpass"
delay=2
for _ in $(seq 1 6); do
  if GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone --depth 1 \
      "https://github.com/${WINGET_GITHUB_USER}/winget-pkgs.git" "$work/repo" 2>/dev/null; then
    break
  fi
  sleep "$delay"
  delay=$((delay < 16 ? delay * 2 : 16))
done
[ -d "$work/repo/.git" ] || { printf 'GitHub fork did not become ready\n' >&2; exit 1; }

branch="qos-imager-${version}"
git -C "$work/repo" switch -c "$branch"
destination="$work/repo/manifests/q/qOS/Imager/$version"
[ ! -e "$destination" ] || { printf 'WinGet already contains qOS.Imager %s\n' "$version"; exit 0; }
mkdir -p "$destination"
cp "$manifest_source"/*.yaml "$destination/"
git -C "$work/repo" config user.name "qOS release automation"
git -C "$work/repo" config user.email "release-bot@qos.dev"
git -C "$work/repo" add "$destination"
git -C "$work/repo" commit -m "New version: qOS.Imager version $version"
GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git -C "$work/repo" push -f origin "$branch"

payload="$(python3 - "$WINGET_GITHUB_USER" "$branch" "$version" <<'PY'
import json, sys
user, branch, version = sys.argv[1:]
print(json.dumps({
    "title": f"New version: qOS.Imager version {version}",
    "head": f"{user}:{branch}",
    "base": "master",
    "body": "Automated submission from the signed qOS Imager release pipeline.",
}))
PY
)"
response="$work/pr.json"
api_call "$response" \
  -H 'Content-Type: application/json' --data "$payload" \
  "$api/repos/microsoft/winget-pkgs/pulls"
case "$API_CODE" in
  201) python3 - "$response" <<'PY'
import json, sys
print("opened " + json.load(open(sys.argv[1], encoding="utf-8"))["html_url"])
PY
  ;;
  422)
    existing="$work/existing-prs.json"
    api_call "$existing" \
      "$api/repos/microsoft/winget-pkgs/pulls?state=open&base=master&head=${WINGET_GITHUB_USER}:${branch}"
    [ "$API_CODE" = 200 ] || { cat "$existing" >&2; exit 1; }
    python3 - "$existing" "$WINGET_GITHUB_USER" "$branch" <<'PY'
import json, sys
path, user, branch = sys.argv[1:]
rows = json.load(open(path, encoding="utf-8"))
matches = [row for row in rows
           if row.get("head", {}).get("ref") == branch
           and row.get("head", {}).get("user", {}).get("login", "").casefold() == user.casefold()]
if len(matches) != 1:
    raise SystemExit("GitHub returned 422, but no unique open WinGet submission PR exists")
print("existing " + matches[0]["html_url"])
PY
    ;;
  *) cat "$response" >&2; exit 1 ;;
esac
