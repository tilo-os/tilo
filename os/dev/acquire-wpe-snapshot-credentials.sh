#!/usr/bin/env bash
# Exchange this job's Forgejo OIDC identity for a short WPE snapshot lease.
# The broker, not this script, validates the signed workflow/ref claims.
set -euo pipefail

mode="${1:-}"
case "$mode" in
  resolve-x86_64|resolve-aarch64|publish-x86_64|publish-aarch64|promote) ;;
  *) printf 'usage: %s {resolve-ARCH|publish-ARCH|promote}\n' "$0" >&2; exit 2 ;;
esac

: "${RUNNER_TEMP:?RUNNER_TEMP is required}"
: "${GITHUB_ENV:?GITHUB_ENV is required}"
: "${GITHUB_WORKSPACE:?GITHUB_WORKSPACE is required}"
container_cli="${QOS_WPE_CONTAINER_CLI:-docker}"
now="$(date +%s)"

bounded_expiry() {
  local expiry="$1" maximum_lifetime="$2" description="$3"
  [[ "$expiry" =~ ^[0-9]+$ ]] \
    || { printf '%s expiry must be a Unix timestamp\n' "$description" >&2; return 1; }
  (( expiry > now )) \
    || { printf '%s has expired\n' "$description" >&2; return 1; }
  (( expiry - now <= maximum_lifetime )) \
    || { printf '%s lifetime exceeds %s seconds\n' "$description" "$maximum_lifetime" >&2; return 1; }
}

trusted_root_executable() {
  local path="$1" description="$2" current mode
  [[ "$path" = /* && -x "$path" && ! -L "$path" ]] \
    || { printf '%s must be an absolute, executable, non-symlink path\n' "$description" >&2; return 1; }
  [ "$(realpath -e "$path")" = "$path" ] \
    || { printf '%s path must be canonical\n' "$description" >&2; return 1; }
  current="$path"
  while :; do
    [ "$(stat -c '%u' "$current")" = 0 ] \
      || { printf '%s and every parent must be owned by root\n' "$description" >&2; return 1; }
    mode="$(stat -c '%a' "$current")"
    (( (8#$mode & 0022) == 0 )) \
      || { printf '%s and every parent must not be group/world writable\n' "$description" >&2; return 1; }
    [ "$current" = / ] && break
    current="$(dirname "$current")"
  done
}

broker="${WPE_SNAPSHOT_CREDENTIAL_BROKER:-}"
if [ -n "$broker" ]; then
  : "${ACTIONS_ID_TOKEN_REQUEST_URL:?Forgejo OIDC request URL is required}"
  : "${ACTIONS_ID_TOKEN_REQUEST_TOKEN:?Forgejo OIDC request token is required}"
  trusted_root_executable "$broker" 'WPE snapshot credential broker'
  case "$broker" in
    "$GITHUB_WORKSPACE"/*|"$RUNNER_TEMP"/*)
      printf 'WPE snapshot credential broker must not come from the checkout or job temporary directory\n' >&2
      exit 1
      ;;
  esac

  lease_dir="$(mktemp -d "$RUNNER_TEMP/qos-wpe-snapshot.XXXXXX")"
  lease="$lease_dir/lease.env"
  trap 'rm -f "$lease"; rmdir "$lease_dir" 2>/dev/null || true' EXIT
  [ ! -e "$lease" ] \
    || { printf 'credential broker lease output must not preexist\n' >&2; exit 1; }
  "$broker" "$mode" "$lease"
  [ -f "$lease" ] && [ ! -L "$lease" ] \
    || { printf 'credential broker did not create a regular lease file\n' >&2; exit 1; }
  [ "$(stat -c '%a' "$lease")" = 600 ] \
    || { printf 'credential broker lease file must have mode 0600\n' >&2; exit 1; }
  [ "$(stat -c '%u' "$lease")" = "$(id -u)" ] \
    || { printf 'credential broker lease file must be owned by the runner account\n' >&2; exit 1; }
  if command -v getfacl >/dev/null 2>&1; then
    [ "$(getfacl -cp "$lease" | grep -Ev '^(#|$)' | sort)" = $'group::---\nother::---\nuser::rw-' ] \
      || { printf 'credential broker lease file has an extended or permissive ACL\n' >&2; exit 1; }
  fi

  registry_username=
  registry_password=
  signer=
  lease_expiry=
  declare -A seen=()
  while IFS= read -r setting || [ -n "$setting" ]; do
    name="${setting%%=*}"
    value="${setting#*=}"
    [[ "$setting" != *$'\r'* && "$setting" =~ ^[A-Z0-9_]+=[^[:cntrl:]]+$ ]] \
      && [ "$name" != "$setting" ] && [ -z "${seen[$name]:-}" ] \
      || { printf 'credential broker returned a malformed or duplicate setting\n' >&2; exit 1; }
    seen[$name]=1
    case "$name" in
      QOS_REGISTRY_USERNAME) registry_username="$value" ;;
      QOS_REGISTRY_PASSWORD) registry_password="$value" ;;
      QOS_WPE_SIGNER) signer="$value" ;;
      QOS_LEASE_EXPIRES_AT) lease_expiry="$value" ;;
      *) printf 'credential broker returned unsupported setting %s\n' "$name" >&2; exit 1 ;;
    esac
  done < "$lease"

  [[ "$registry_username" =~ ^[A-Za-z0-9._@+-]+$ ]] && [ -n "$registry_password" ] \
    || { printf 'credential broker returned invalid registry credentials\n' >&2; exit 1; }
  bounded_expiry "$lease_expiry" 900 'credential broker lease'

  if [[ "$mode" = publish-* ]]; then
    [ "${#seen[@]}" = 4 ] \
      || { printf 'publish lease must return exactly four settings\n' >&2; exit 1; }
    [[ "$signer" =~ ^/[A-Za-z0-9._/+:-]+$ ]] \
      || { printf 'credential broker did not return an absolute executable signer\n' >&2; exit 1; }
    trusted_root_executable "$signer" 'WPE signer'
    case "$signer" in
      "$GITHUB_WORKSPACE"/*|"$RUNNER_TEMP"/*)
        printf 'WPE signer must not come from the checkout or job temporary directory\n' >&2
        exit 1
        ;;
    esac
    printf 'QOS_WPE_SIGNER=%s\n' "$signer" >> "$GITHUB_ENV"
  else
    [ "${#seen[@]}" = 3 ] && [ -z "${seen[QOS_WPE_SIGNER]+x}" ] \
      || { printf 'non-publish lease must return exactly three settings without a signer\n' >&2; exit 1; }
  fi

  printf '%s' "$registry_password" | "$container_cli" login code.sandbox.example \
    --username "$registry_username" --password-stdin
  registry_password=
  rm -f "$lease"
  rmdir "$lease_dir"
  trap - EXIT
  exit 0
fi

[ "${WPE_ALLOW_STATIC_CREDENTIAL_BOOTSTRAP:-}" = 1 ] \
  || { printf 'configure WPE_SNAPSHOT_CREDENTIAL_BROKER; static credentials are disabled\n' >&2; exit 1; }
bounded_expiry "${WPE_STATIC_BOOTSTRAP_EXPIRES_AT:-}" 86400 'static credential bootstrap'
[ -n "${REGISTRY_TOKEN:-}" ] \
  || { printf 'static credential bootstrap requires REGISTRY_TOKEN\n' >&2; exit 1; }
if [[ "$mode" = publish-* ]]; then
  printf 'QOS_WPE_STATIC_BOOTSTRAP=1\n' >> "$GITHUB_ENV"
fi
printf '%s' "$REGISTRY_TOKEN" | "$container_cli" login code.sandbox.example \
  --username "${GITHUB_ACTOR:?GITHUB_ACTOR is required}" --password-stdin
