#!/usr/bin/env bash
# The upgrade a release of tilo.sh makes: this checkout's relay started on the
# Lounge the release the live relay runs kept.
#
#   tools/relay-upgrade/run.sh release RELEASE OUT   (t test relay-upgrade-release)
#   tools/relay-upgrade/run.sh upgrade RELEASE OUT   (t test relay-upgrade)
#
# RELEASE is a directory beside this script: `source` pins the release's commit
# and the SHA-256 of its source archive, and `seed.rs` writes a Lounge with that
# release's own relay and its own test harness.
#
# `release` fetches the release's source from the forge, checked against the
# pin, builds its relay tests with `seed.rs` added and runs the seed: the
# release's relay keeps a Lounge (its owner and moderator, people at their
# roles, a pending invitation, a member's own listed workspace, the door's
# settings, a chat record). It leaves that state in OUT/state.tar and the
# release's relay in OUT/tilo-relay. Its result is kept as long as this
# directory is unchanged, so the release is built once.
#
# `upgrade` starts this checkout's relay on a copy of that state and checks all
# of it is there and works (`persistent_room::release_upgrade`); a refusal or a
# loss fails it. It then starts the release's relay on the state this checkout
# left and says whether a rollback could run on it without a restore. That part
# reports and never fails: the planned rollback is the backup taken before.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
mode=${1:?usage: run.sh release|upgrade RELEASE OUT}
release=${2:?usage: run.sh release|upgrade RELEASE OUT}
out=${3:?usage: run.sh release|upgrade RELEASE OUT}
pin=$here/$release
[[ -f $pin/source && -f $pin/seed.rs ]] || { echo "relay-upgrade: $pin needs source and seed.rs" >&2; exit 2; }

say() { printf 'relay-upgrade: %s\n' "$*" >&2; }

# The test's output, passed on; fails unless exactly one test ran and passed,
# so a name no test answers to is no pass. The Lounge's tests are built with the
# relay's `agents` feature, as the image's relay is.
ran_one() {
  local line passed=''
  while IFS= read -r line; do
    printf '%s\n' "$line"
    [[ $line == 'test result: ok. 1 passed;'* ]] && passed=1
  done
  [[ -n $passed ]]
}

release() {
  # shellcheck source=/dev/null
  . "$pin/source"
  [[ ${commit:-} =~ ^[0-9a-f]{40}$ && ${tar_sha256:-} =~ ^[0-9a-f]{64}$ ]] \
    || { say "$pin/source names no commit and tar_sha256"; exit 2; }
  local cache=${CARGO_TARGET_DIR:-$repo/target}/relay-upgrade/$release
  local tree=$cache/tilo
  if [[ ! -f $cache/$commit.unpacked ]]; then
    say "fetching $release ($commit)"
    rm -rf "$tree" "$cache/$commit.tar"
    mkdir -p "$cache"
    curl -fsS --retry 3 --max-time 300 "https://code.tilo.so/tilo/tilo/archive/$commit.tar.gz" \
      | gzip -dc > "$cache/$commit.tar"
    local got
    got=$(sha256sum "$cache/$commit.tar" | cut -d' ' -f1)
    [[ $got == "$tar_sha256" ]] || { say "the forge's archive of $commit is $got, not $tar_sha256"; exit 1; }
    tar -xf "$cache/$commit.tar" -C "$cache"
    rm "$cache/$commit.tar"
    cp "$tree/crates/hosting/tilo-relay/tests/persistent_room.rs" "$cache/persistent_room.rs.release"
    touch "$cache/$commit.unpacked"
  fi
  # The release's relay tests with the seed added, written only when they
  # change, so cargo rebuilds nothing it already built.
  local seeded
  seeded=$(mktemp)
  cat "$cache/persistent_room.rs.release" "$pin/seed.rs" > "$seeded"
  cmp -s "$seeded" "$tree/crates/hosting/tilo-relay/tests/persistent_room.rs" \
    || cp "$seeded" "$tree/crates/hosting/tilo-relay/tests/persistent_room.rs"
  rm -f "$seeded"

  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$work'" EXIT
  export TILO_UPGRADE_DIR=$work/state
  mkdir -p "$TILO_UPGRADE_DIR"
  say "$release keeps a Lounge"
  (cd "$tree" && CARGO_TARGET_DIR=$cache/target cargo test --locked -p tilo-relay --features agents \
    --test it -- --ignored --exact persistent_room::release_seed::a_lounge_the_release_keeps \
    --nocapture --test-threads=1) | ran_one
  cat "$TILO_UPGRADE_DIR/seed.json" >&2
  say "what $release kept:"
  (cd "$TILO_UPGRADE_DIR" && find . -type f -printf '%P %s\n' | sort) >&2
  rm -rf "$out"
  mkdir -p "$out"
  tar -cf "$out/state.tar" -C "$TILO_UPGRADE_DIR" .
  install -m 0755 "$cache/target/debug/tilo-relay" "$out/tilo-relay"
}

# The release's relay on the state this checkout's relay left, as eu1 would
# start it after a rollback: whether it opens the Lounge and lists its people.
rollback() {
  local state=$1 relay=$2 expected=$3
  local plain
  plain=$(python3 -I -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])')
  local log=$state/rollback.log
  local args=(room --state "$state/lounge/relay" --web 127.0.0.1:0 --tls dev --persistent plaza
    --root-file "$state/root" --room-id-file "$state/lounge/room-id" --seed-app so.tilo.chat
    --member-limits --workspace-name Lounge --listing-description
    'The Tilo community: chat, notes and a shared desktop' --default-role editor
    --chat-channel general --chat-channel feedback --feedback-channel feedback
    --roles-file "$state/roles.json" --invitation-only --wait-list-max 500
    --web-plain "127.0.0.1:$plain")
  # Its operator's input stays open while it runs: at its end the room stops.
  TILO_SECRETS_DIR=$state/lounge/secrets TILO_RELAY_LOG=warn RUST_BACKTRACE=0 "$relay" "${args[@]}" \
    < <(sleep 120) > "$log" 2>&1 &
  local pid=$! answer='' deadline=$((SECONDS + 90))
  while ((SECONDS < deadline)) && kill -0 "$pid" 2>/dev/null; do
    answer=$(curl -fsS -X POST "http://127.0.0.1:$plain/peer/v1/directory" 2>/dev/null) && break
    sleep 1
  done
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  if [[ -z $answer ]]; then
    say "rollback: $release does not open the state this checkout left; going back means restoring the backup taken before the upgrade. Its last words:"
    tail -15 "$log" >&2
    return
  fi
  local members
  members=$(python3 -I -c 'import json, sys; page = json.load(sys.stdin); print(" ".join(f"{w[\"name\"]}={w[\"members\"]}" for w in page["workspaces"]))' <<< "$answer")
  say "rollback: $release opens the state this checkout left and lists: $members (the Lounge had $expected before the upgrade)"
}

upgrade() {
  [[ -f $out/state.tar && -x $out/tilo-relay ]] \
    || { say "$out holds no state.tar and tilo-relay: run.sh release $release $out first"; exit 2; }
  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$work'" EXIT
  export TILO_UPGRADE_DIR=$work/state
  mkdir -p "$TILO_UPGRADE_DIR"
  tar -xf "$out/state.tar" -C "$TILO_UPGRADE_DIR"
  say "this checkout's relay opens the Lounge $release kept"
  (cd "$repo" && TILO_UPGRADE_RELAY_LOG=${TILO_UPGRADE_RELAY_LOG:-warn} cargo test --locked -p tilo-relay --features agents --test it -- \
    --ignored --exact persistent_room::release_upgrade::the_lounge_a_release_kept_starts_with_everything_in_it \
    --nocapture --test-threads=1) | ran_one
  say "the upgrade kept the Lounge"
  local expected
  expected=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["listed_members"])' "$TILO_UPGRADE_DIR/seed.json")
  rollback "$TILO_UPGRADE_DIR" "$out/tilo-relay" "$expected"
}

case $mode in
  release) release ;;
  upgrade) upgrade ;;
  *) say "run.sh release|upgrade RELEASE OUT, not $mode"; exit 2 ;;
esac
