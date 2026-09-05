#!/usr/bin/env bash
# Launch exactly one qOS Imager job on a disposable Linux or macOS host.
# Install this file and forgejo-runner root-owned on the golden image; do not
# execute the copy from a workflow checkout.
set -euo pipefail

label="${1:-}"
uuid_file="${2:-}"
token_file="${3:-}"
[ "$#" -eq 3 ] || {
  printf 'usage: %s LABEL UUID_FILE TOKEN_FILE\n' "$0" >&2
  exit 2
}

runner="${QOS_FORGEJO_RUNNER:-/usr/local/bin/forgejo-runner}"
server="${QOS_FORGEJO_URL:-https://code.podesta.ai}"
max_wait="${QOS_RUNNER_MAX_WAIT_SECONDS:-21600}"

stat_owner_mode() {
  if stat -c '%u %a' "$1" >/dev/null 2>&1; then
    stat -c '%u %a' "$1"
  else
    stat -f '%u %Lp' "$1"
  fi
}

trusted_program() {
  local path="$1" current owner mode
  [[ "$path" = /* && -f "$path" && -x "$path" && ! -L "$path" ]] || {
    printf 'forgejo-runner must be an absolute executable non-symlink\n' >&2
    return 1
  }
  [ "$(realpath -e "$path")" = "$path" ] || {
    printf 'forgejo-runner path must be canonical\n' >&2
    return 1
  }
  current="$path"
  while :; do
    read -r owner mode < <(stat_owner_mode "$current")
    [ "$owner" = 0 ] && (( (8#$mode & 0022) == 0 )) || {
      printf 'forgejo-runner and every parent must be root-owned and not group/world writable\n' >&2
      return 1
    }
    [ "$current" = / ] && break
    current="$(dirname "$current")"
  done
}

private_metadata_file() {
  local path="$1" description="$2" owner mode current parent_owner parent_mode
  [[ "$path" = /* && -f "$path" && ! -L "$path" ]] || {
    printf '%s must be an absolute regular non-symlink\n' "$description" >&2
    return 1
  }
  [ "$(realpath -e "$path")" = "$path" ] || {
    printf '%s path must be canonical\n' "$description" >&2
    return 1
  }
  read -r owner mode < <(stat_owner_mode "$path")
  [ "$owner" = "$(id -u)" ] && { [ "$mode" = 600 ] || [ "$mode" = 400 ]; } || {
    printf '%s must be runner-owned mode 0400 or 0600\n' "$description" >&2
    return 1
  }
  current="$(dirname "$path")"
  read -r parent_owner parent_mode < <(stat_owner_mode "$current")
  [ "$parent_owner" = "$(id -u)" ] \
    && (( (8#$parent_mode & 0300) == 0300 )) \
    && (( (8#$parent_mode & 0077) == 0 )) || {
      printf '%s immediate parent must be a private, runner-owned writable directory\n' "$description" >&2
      return 1
    }
  while :; do
    read -r parent_owner parent_mode < <(stat_owner_mode "$current")
    { [ "$parent_owner" = 0 ] || [ "$parent_owner" = "$(id -u)" ]; } \
      && (( (8#$parent_mode & 0022) == 0 )) || {
        printf '%s parent directories must not be replaceable by another user\n' "$description" >&2
        return 1
      }
    [ "$current" = / ] && break
    current="$(dirname "$current")"
  done
}

process_group_has_live_members() {
  local proc_stat stat_line stat_tail state parent process_group
  # A container without an init process can leave an orphaned descendant as a
  # zombie indefinitely.  `kill -0` still reports that process, even though it
  # cannot execute and has already released its resources.  On Linux, inspect
  # procfs with Bash built-ins so cleanup verifies live members without taking
  # a dependency on the host-specific `ps` utility or its output format.
  if [ -r /proc/self/stat ]; then
    for proc_stat in /proc/[0-9]*/stat; do
      IFS= read -r stat_line 2>/dev/null < "$proc_stat" || continue
      stat_tail="${stat_line##*) }"
      [ "$stat_tail" != "$stat_line" ] || continue
      read -r state parent process_group _ <<< "$stat_tail"
      if [ "$process_group" = "$1" ] && [ "$state" != Z ]; then
        return 0
      fi
    done
    return 1
  fi
  # Bash's job-control mode below starts each background job in a process
  # group whose ID is its leader PID.  Probe that group with the shell builtin
  # on platforms without procfs.
  kill -0 -- "-$1" 2>/dev/null
}

private_metadata_file "$uuid_file" 'runner UUID file'
private_metadata_file "$token_file" 'runner token file'

runner_pid=
runner_pgid=
runner_reaped=0
watcher_pid=
watcher_pgid=
cleanup() {
  local status=$? cleanup_failed=0 attempt
  trap - EXIT INT TERM HUP
  if [ -n "$watcher_pid" ]; then
    if [ -n "$watcher_pgid" ]; then
      kill -TERM -- "-$watcher_pgid" 2>/dev/null || true
    else
      kill "$watcher_pid" 2>/dev/null || true
    fi
    wait "$watcher_pid" 2>/dev/null || true
  fi
  if [ -n "$runner_pgid" ] && process_group_has_live_members "$runner_pgid"; then
    kill -TERM -- "-$runner_pgid" 2>/dev/null || true
    for attempt in {1..50}; do
      process_group_has_live_members "$runner_pgid" || break
      sleep 0.1
    done
    if process_group_has_live_members "$runner_pgid"; then
      kill -KILL -- "-$runner_pgid" 2>/dev/null || true
      for attempt in {1..50}; do
        process_group_has_live_members "$runner_pgid" || break
        sleep 0.1
      done
    fi
  fi
  if [ -n "$runner_pid" ] && [ "$runner_reaped" = 0 ]; then
    wait "$runner_pid" 2>/dev/null || true
  fi
  if [ -n "$runner_pgid" ] && process_group_has_live_members "$runner_pgid"; then
    printf 'ephemeral runner process group survived cleanup\n' >&2
    cleanup_failed=1
  fi
  rm -f -- "$uuid_file" "$token_file" || cleanup_failed=1
  if [ -e "$uuid_file" ] || [ -e "$token_file" ]; then
    printf 'ephemeral runner registration metadata survived cleanup\n' >&2
    cleanup_failed=1
  fi
  [ "$cleanup_failed" = 0 ] || { [ "$status" -ne 0 ] || status=1; }
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

[ "$uuid_file" != "$token_file" ] || { printf 'UUID and token paths must differ\n' >&2; exit 1; }
case "$server" in
  https://code.podesta.ai|https://code.podesta.ai/) ;;
  *) printf 'Forgejo URL must be the pinned qOS origin\n' >&2; exit 1 ;;
esac
[[ "$max_wait" =~ ^[1-9][0-9]{0,5}$ ]] && [ "$max_wait" -le 86400 ] || {
  printf 'runner wait timeout must be between 1 and 86400 seconds\n' >&2
  exit 1
}

host_os="$(uname -s)"
host_arch="$(uname -m)"
case "$host_arch" in
  x86_64|amd64) host_arch=x86_64 ;;
  aarch64|arm64) host_arch=aarch64 ;;
  *) printf 'unsupported native runner architecture: %s\n' "$host_arch" >&2; exit 1 ;;
esac
case "$label:$host_os:$host_arch" in
  imager-linux-x86_64:Linux:x86_64|imager-linux-aarch64:Linux:aarch64|imager-macos-x86_64:Darwin:x86_64|imager-macos-aarch64:Darwin:aarch64) ;;
  *) printf 'runner label %s does not match native host %s/%s\n' "$label" "$host_os" "$host_arch" >&2; exit 1 ;;
esac
trusted_program "$runner"

for metadata in "$uuid_file" "$token_file"; do
  [ "$(wc -l < "$metadata" | tr -d ' ')" = 0 ] || {
    printf 'runner metadata must contain one value without a newline\n' >&2
    exit 1
  }
done
uuid="$(cat "$uuid_file")"
token="$(cat "$token_file")"
[[ "$uuid" =~ ^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$ ]] || {
  printf 'runner metadata contains an invalid UUID\n' >&2
  exit 1
}
[[ "$token" =~ ^[0-9a-fA-F]{40}$ ]] || {
  printf 'runner metadata contains an invalid one-job token\n' >&2
  exit 1
}
unset token

version="$($runner --version 2>&1)"
if [[ "$version" =~ ([0-9]+)\.([0-9]+)\.([0-9]+) ]]; then
  major="${BASH_REMATCH[1]}"; minor="${BASH_REMATCH[2]}"
else
  printf 'could not determine forgejo-runner version\n' >&2
  exit 1
fi
if (( major < 12 || (major == 12 && minor < 5) )); then
  printf 'forgejo-runner 12.5 or newer is required for one-job mode\n' >&2
  exit 1
fi

set -m
"$runner" one-job \
  --url "${server%/}" \
  --uuid "$uuid" \
  --token-url "file://$token_file" \
  --label "$label:host" \
  --wait &
runner_pid=$!
# `set -m` gives this background job its own group, led by `$!`.  This remains
# valid even when a very short one-job runner exits before the next statement.
runner_pgid="$runner_pid"
(
  sleep "$max_wait"
  kill -TERM -- "-$runner_pgid" 2>/dev/null || exit 0
  sleep 10
  kill -KILL -- "-$runner_pgid" 2>/dev/null || true
) &
watcher_pid=$!
watcher_pgid="$watcher_pid"

set +e
wait "$runner_pid"
status=$?
set -e
runner_reaped=1
kill -TERM -- "-$watcher_pgid" 2>/dev/null || true
wait "$watcher_pid" 2>/dev/null || true
watcher_pid=
watcher_pgid=
if [ "$status" -ne 0 ]; then
  printf 'ephemeral Forgejo runner exited unsuccessfully (%s)\n' "$status" >&2
  exit "$status"
fi
printf 'one-job runner completed; destroy this VM now\n'
