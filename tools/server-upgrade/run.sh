#!/usr/bin/env bash
# The people a release of tilo.sh kept, carried into the release that starts its
# relay on fresh state.
#
#   tools/server-upgrade/run.sh release RELEASE OUT   (t test server-upgrade-release)
#   tools/server-upgrade/run.sh upgrade RELEASE OUT   (t test server-upgrade)
#
# RELEASE is a directory beside this script: `source` pins the release's commit
# and the SHA-256 of its source archive, and `seed.rs` writes the state that
# release's own relay leaves, with that release's own test harness.
#
# `release` fetches the release's source from the forge, checked against the
# pin, builds its relay tests with `seed.rs` added and runs the seed: the
# release's relay keeps a Lounge (its people, one of them invited at an address,
# a workspace of one member's own, a chat one of them wrote in, the entry's
# settings), and the state it leaves is OUT/state.tar. Its result is kept as
# long as this directory is unchanged, so the release is built once.
#
# `upgrade` unpacks that state and runs this checkout's check
# (`persistent_workspace::release_upgrade`): the export reads the people out of
# the release's state without writing to it, this checkout's relay starts on its
# own new directory of the same volume, and the import takes the people in —
# their addresses on the wait list, an invitation mailed to each of them.
set -euo pipefail

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
mode=${1:?usage: run.sh release|upgrade RELEASE OUT}
release=${2:?usage: run.sh release|upgrade RELEASE OUT}
out=${3:?usage: run.sh release|upgrade RELEASE OUT}
pin=$here/$release
[[ -f $pin/source && -f $pin/seed.rs ]] || { echo "server-upgrade: $pin needs source and seed.rs" >&2; exit 2; }

say() { printf 'server-upgrade: %s\n' "$*" >&2; }

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
  local cache=${CARGO_TARGET_DIR:-$repo/target}/server-upgrade/$release
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
  export TILO_UPGRADE_DIR=$work
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
}

upgrade() {
  [[ -f $out/state.tar ]] \
    || { say "$out holds no state.tar: run.sh release $release $out first"; exit 2; }
  local work
  work=$(mktemp -d)
  # shellcheck disable=SC2064
  trap "rm -rf '$work'" EXIT
  export TILO_UPGRADE_DIR=$work
  tar -xf "$out/state.tar" -C "$TILO_UPGRADE_DIR"
  say "this checkout's release takes the people $release kept in"
  (cd "$repo" && TILO_UPGRADE_SERVER_LOG=${TILO_UPGRADE_SERVER_LOG:-warn} cargo test --locked -p tilo-server --features agents --test it -- \
    --ignored --exact persistent_workspace::release_upgrade::the_people_a_release_kept_arrive_in_the_release_that_starts_fresh \
    --nocapture --test-threads=1) | ran_one
  say "the people $release kept arrived"
}

case $mode in
  release) release ;;
  upgrade) upgrade ;;
  *) say "run.sh release|upgrade RELEASE OUT, not $mode"; exit 2 ;;
esac
