#!/usr/bin/env bash
# Acquire one short-lived publication token without accepting attacker-shaped files.
set -euo pipefail
kind="${1:-}"
broker="${2:-}"
output="${3:-}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
[[ "$kind" =~ ^(release|homebrew|winget|site|internal-bundle)$ ]] && [[ "$output" = "$RUNNER_TEMP"/* ]] \
  || { printf 'usage: %s {release|homebrew|winget|site|internal-bundle} BROKER RUNNER_TEMP/OUTPUT\n' "$0" >&2; exit 2; }
[[ "$broker" = /* && -x "$broker" && ! -L "$broker" ]] \
  || { printf 'credential broker must be an absolute executable non-symlink\n' >&2; exit 1; }
[ "$(realpath "$broker")" = "$broker" ] \
  || { printf 'credential broker must be canonical\n' >&2; exit 1; }
case "$broker" in "$GITHUB_WORKSPACE"/*|"$RUNNER_TEMP"/*) printf 'credential broker is in an untrusted directory\n' >&2; exit 1;; esac
current="$broker"
while :; do
  if stat -c '%u %a' "$current" >/dev/null 2>&1; then read -r owner mode < <(stat -c '%u %a' "$current"); else read -r owner mode < <(stat -f '%u %Lp' "$current"); fi
  [ "$owner" = 0 ] && (( (8#$mode & 0022) == 0 )) \
    || { printf 'credential broker and parents must be root-owned and not writable by other users\n' >&2; exit 1; }
  [ "$current" = / ] && break; current="$(dirname "$current")"
done
: "${ACTIONS_ID_TOKEN_REQUEST_URL:?Forgejo OIDC request URL is required}"
: "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?Forgejo OIDC request token is required}"
[ ! -e "$output" ] || { printf 'credential output must not preexist\n' >&2; exit 1; }
"$broker" "$kind" "$output" >/dev/null
[ -f "$output" ] && [ ! -L "$output" ] || { printf 'credential broker did not create a regular token file\n' >&2; exit 1; }
if stat -c '%u %a' "$output" >/dev/null 2>&1; then read -r owner mode < <(stat -c '%u %a' "$output"); else read -r owner mode < <(stat -f '%u %Lp' "$output"); fi
[ "$owner" = "$(id -u)" ] && [ "$mode" = 600 ] || { printf 'token file must be runner-owned mode 0600\n' >&2; exit 1; }
if stat -c '%u' "$output" >/dev/null 2>&1 && command -v getfacl >/dev/null 2>&1; then
  [ "$(getfacl -cp "$output" | grep -Ev '^(#|$)' | sort)" = $'group::---\nother::---\nuser::rw-' ] \
    || { printf 'token file has an extended or permissive ACL\n' >&2; exit 1; }
elif ! stat -c '%u' "$output" >/dev/null 2>&1; then
  [ "$(ls -lde "$output" | wc -l | tr -d ' ')" = 1 ] \
    || { printf 'token file has an extended ACL\n' >&2; exit 1; }
fi
lines="$(wc -l < "$output" | tr -d ' ')"
[ "$lines" = 0 ] && [ -s "$output" ] && LC_ALL=C grep -Eq '^[A-Za-z0-9._~+/=-]+$' "$output" \
  || { printf 'credential broker returned an empty, multiline, or malformed token\n' >&2; exit 1; }
