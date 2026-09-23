#!/usr/bin/env bash
# Authenticate non-interactively while tolerating a short registry outage.
# Authentication/policy failures are final; only explicit transport/5xx/429
# failures receive the bounded retry budget.
set -euo pipefail

if [ "$#" -ne 3 ]; then
  printf 'usage: %s CONTAINER_CLI REGISTRY USERNAME < password\n' "$0" >&2
  exit 2
fi

container_cli=$1
registry=$2
username=$3
command -v "$container_cli" >/dev/null 2>&1 || {
  printf '%s is required for registry login\n' "$container_cli" >&2
  exit 2
}
case "$registry" in
  ''|*[!A-Za-z0-9.:-]*) printf 'registry name is invalid\n' >&2; exit 2 ;;
esac
[ -n "$username" ] || { printf 'registry username is required\n' >&2; exit 2; }

password=$(cat)
[ -n "$password" ] || { printf 'registry password is required\n' >&2; exit 2; }

retry_delays=${TILO_REGISTRY_LOGIN_RETRY_DELAYS:-2 5 10 20}
read -r -a delays <<< "$retry_delays"
[ "${#delays[@]}" -le 6 ] || {
  printf 'registry login retry schedule is too long\n' >&2
  exit 2
}
for delay in "${delays[@]}"; do
  [[ "$delay" =~ ^[0-9]+$ ]] && [ "$delay" -le 60 ] || {
    printf 'registry login retry delay is invalid\n' >&2
    exit 2
  }
done

login_log=$(mktemp)
trap 'rm -f "$login_log"' EXIT
attempt=1
while :; do
  : > "$login_log"
  if printf '%s' "$password" | "$container_cli" login "$registry" \
      --username "$username" --password-stdin >"$login_log" 2>&1; then
    cat "$login_log"
    exit 0
  fi

  # Never retry bad credentials, authorization policy, malformed requests, or
  # an unclassified error. Those require intervention rather than extra load.
  if ! grep -Eiq \
      'status: (429|5[0-9][0-9])|too many requests|service unavailable|bad gateway|gateway timeout|connection (refused|reset)|i/o timeout|tls handshake timeout|unexpected eof|temporary failure|no route to host|server misbehaving' \
      "$login_log"; then
    cat "$login_log" >&2
    exit 1
  fi

  delay_index=$((attempt - 1))
  if [ "$delay_index" -ge "${#delays[@]}" ]; then
    cat "$login_log" >&2
    printf 'registry login still unavailable after %d attempts\n' "$attempt" >&2
    exit 1
  fi
  delay=${delays[$delay_index]}
  printf 'registry login transiently unavailable; retrying in %ss (%d/%d)\n' \
    "$delay" "$attempt" "$(( ${#delays[@]} + 1 ))" >&2
  sleep "$delay"
  attempt=$((attempt + 1))
done
