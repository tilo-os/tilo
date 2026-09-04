#!/usr/bin/env bash
# Validate the narrow, path-only lease returned by a native release signer broker.
set -euo pipefail

platform="${1:-}"
broker="${2:-}"
: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"

trusted_path() {
  local path="$1" kind="$2" description="$3" current mode owner
  [[ "$path" = /* && ! -L "$path" ]] \
    || { printf '%s must be an absolute, non-symlink path\n' "$description" >&2; return 1; }
  [ "$(realpath -e "$path")" = "$path" ] \
    || { printf '%s must be canonical\n' "$description" >&2; return 1; }
  case "$path" in
    "$GITHUB_WORKSPACE"/*|"$RUNNER_TEMP"/*)
      printf '%s must not come from the checkout or job temporary directory\n' "$description" >&2
      return 1
      ;;
  esac
  if [ "$kind" = executable ]; then
    [ -f "$path" ] && [ -x "$path" ]
  else
    [ -f "$path" ] && [ -r "$path" ]
  fi || { printf '%s has the wrong file type or access mode\n' "$description" >&2; return 1; }
  current="$path"
  while :; do
    if stat -c '%u %a' "$current" >/dev/null 2>&1; then
      read -r owner mode < <(stat -c '%u %a' "$current")
    else
      read -r owner mode < <(stat -f '%u %Lp' "$current")
    fi
    [ "$owner" = 0 ] \
      || { printf '%s and every parent must be owned by root\n' "$description" >&2; return 1; }
    (( (8#$mode & 0022) == 0 )) \
      || { printf '%s and every parent must not be group/world writable\n' "$description" >&2; return 1; }
    [ "$current" = / ] && break
    current="$(dirname "$current")"
  done
}

private_regular_file() {
  local path="$1" mode owner
  [ -f "$path" ] && [ ! -L "$path" ] \
    || { printf 'credential broker did not create a regular lease file\n' >&2; return 1; }
  if stat -c '%u %a' "$path" >/dev/null 2>&1; then
    read -r owner mode < <(stat -c '%u %a' "$path")
    if command -v getfacl >/dev/null 2>&1; then
      [ "$(getfacl -cp "$path" | grep -Ev '^(#|$)' | sort)" = $'group::---\nother::---\nuser::rw-' ] \
        || { printf 'credential broker lease file has an extended or permissive ACL\n' >&2; return 1; }
    fi
  else
    read -r owner mode < <(stat -f '%u %Lp' "$path")
    [ "$(ls -lde "$path" | wc -l | tr -d ' ')" = 1 ] \
      || { printf 'credential broker lease file has an extended ACL\n' >&2; return 1; }
  fi
  [ "$owner" = "$(id -u)" ] && [ "$mode" = 600 ] \
    || { printf 'credential broker lease file must be runner-owned mode 0600\n' >&2; return 1; }
}

case "$platform" in linux|macos) ;; *) printf 'usage: %s {linux|macos} BROKER\n' "$0" >&2; exit 2 ;; esac
trusted_path "$broker" executable 'credential broker'
: "${ACTIONS_ID_TOKEN_REQUEST_URL:?Forgejo OIDC request URL is required}"
: "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?Forgejo OIDC request token is required}"

lease_dir="$(mktemp -d "$RUNNER_TEMP/qos-imager-$platform.XXXXXX")"
chmod 0700 "$lease_dir"
lease="$lease_dir/lease.env"
trap 'rm -rf -- "$lease_dir"' EXIT
[ ! -e "$lease" ] || { printf 'credential lease output already exists\n' >&2; exit 1; }
"$broker" "$platform" "$lease"
private_regular_file "$lease"

declare -A values=()
while IFS= read -r setting || [ -n "$setting" ]; do
  [[ "$setting" != *$'\r'* && "$setting" =~ ^([A-Z0-9_]+)=([^[:cntrl:]]+)$ ]] \
    || { printf 'credential broker returned a malformed setting\n' >&2; exit 1; }
  name="${BASH_REMATCH[1]}"; value="${BASH_REMATCH[2]}"
  [ -z "${values[$name]+x}" ] \
    || { printf 'credential broker returned duplicate setting %s\n' "$name" >&2; exit 1; }
  values[$name]="$value"
done < "$lease"

if [ "$platform" = linux ]; then
  [ "${#values[@]}" = 2 ] && [ -n "${values[QOS_LINUX_SIGNER]:-}" ] \
    && [ -n "${values[QOS_LINUX_VERIFY_KEY]:-}" ] \
    || { printf 'Linux broker must return exactly signer and verification-key paths\n' >&2; exit 1; }
  trusted_path "${values[QOS_LINUX_SIGNER]}" executable 'Linux signer'
  trusted_path "${values[QOS_LINUX_VERIFY_KEY]}" file 'Linux verification key'
  printf 'QOS_LINUX_SIGNER=%s\nQOS_LINUX_VERIFY_KEY=%s\n' \
    "${values[QOS_LINUX_SIGNER]}" "${values[QOS_LINUX_VERIFY_KEY]}"
else
  [ "${#values[@]}" = 2 ] && [ -n "${values[QOS_MAC_SIGNING_IDENTITY]:-}" ] \
    && [ -n "${values[QOS_NOTARY_KEYCHAIN_PROFILE]:-}" ] \
    || { printf 'macOS broker must return exactly a signing identity and notary profile\n' >&2; exit 1; }
  identity_re='^[A-Za-z0-9][A-Za-z0-9 .,:()/_+-]{0,255}$'
  [[ "${values[QOS_MAC_SIGNING_IDENTITY]}" =~ $identity_re ]]
  [[ "${values[QOS_NOTARY_KEYCHAIN_PROFILE]}" =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]]
  printf 'QOS_MAC_SIGNING_IDENTITY=%s\nQOS_NOTARY_KEYCHAIN_PROFILE=%s\n' \
    "${values[QOS_MAC_SIGNING_IDENTITY]}" "${values[QOS_NOTARY_KEYCHAIN_PROFILE]}"
fi
