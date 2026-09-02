#!/usr/bin/env bash
# Submit the generated qOS.Imager manifest to Microsoft's public WinGet index.
set -euo pipefail

: "${GITHUB_REF_NAME:?missing release tag}"
: "${WINGET_GITHUB_TOKEN:?missing protected GitHub token}"
: "${WINGET_GITHUB_USER:?missing GitHub account owning the submission fork}"

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
if ! curl --fail --silent -H "$auth" "$api/repos/${WINGET_GITHUB_USER}/winget-pkgs" >/dev/null; then
  curl --fail --silent --show-error -X POST -H "$auth" -H 'Content-Type: application/json' \
    --data '{}' \
    "$api/repos/microsoft/winget-pkgs/forks" >/dev/null
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
askpass="$work/askpass.sh"
printf '#!/bin/sh\nprintf "%%s" "$WINGET_GITHUB_TOKEN"\n' > "$askpass"
chmod 0700 "$askpass"
for _ in $(seq 1 12); do
  if GIT_ASKPASS="$askpass" GIT_TERMINAL_PROMPT=0 git clone --depth 1 \
      "https://github.com/${WINGET_GITHUB_USER}/winget-pkgs.git" "$work/repo" 2>/dev/null; then
    break
  fi
  sleep 5
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
code="$(curl --silent --show-error -o "$response" -w '%{http_code}' -H "$auth" \
  -H 'Content-Type: application/json' --data "$payload" \
  "$api/repos/microsoft/winget-pkgs/pulls")"
case "$code" in
  201) python3 - "$response" <<'PY'
import json, sys
print("opened " + json.load(open(sys.argv[1], encoding="utf-8"))["html_url"])
PY
  ;;
  422) printf 'WinGet submission PR already exists for %s\n' "$version" ;;
  *) cat "$response" >&2; exit 1 ;;
esac
