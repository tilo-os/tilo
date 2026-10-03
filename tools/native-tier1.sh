#!/usr/bin/env bash
# Run the native data plane's privileged acceptance suites on loop-mounted
# Btrfs store volumes: `fs --test native_tier1` (two daemons on two stores:
# placeholders, leases, SQLite, git sync, crash recovery) and
# `fs-native --test it` (capture, journal, pre-content hydration).
#
#   tools/native-tier1.sh [TEST_ARGS...]
#
# TEST_ARGS go to every test binary (a name filter, `--exact`,
# `--nocapture`). The suites run one test at a time, as root: the script
# uses `sudo -n` when it is not root already. Builds use `$TILO_CARGO`
# (default `cargo`; a drop-in that builds remotely and fetches the binaries
# works too), so only the tests have to run on this machine. TILO_REQUIRE_FANOTIFY=1 is set: a
# machine that cannot run a test fails it instead of skipping it.
#
# Needs: Linux with pre-content fanotify events (6.14 or later, with
# CONFIG_FANOTIFY_ACCESS_PERMISSIONS), Btrfs and loop devices, root,
# `sqlite3` and `git` on PATH.
#
# Environment:
#   TILO_CARGO          cargo or a drop-in for it (default cargo)
#   TILO_NATIVE_TIER1_DIR  where the store images and mounts live (default
#                       ~/.cache/tilo/native-tier1; removed afterwards). The
#                       tests' own directories go to a short-lived directory
#                       below /var/tmp: daemon socket paths are bounded
#                       (SUN_LEN).
#   TILO_NATIVE_TIER1_TARGETS  PACKAGE:TEST pairs to run
#                       (default "fs:native_tier1 fs-native:it")
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cargo=${TILO_CARGO:-cargo}
work=${TILO_NATIVE_TIER1_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tilo/native-tier1}
targets=${TILO_NATIVE_TIER1_TARGETS:-fs:native_tier1 fs-native:it}

as_root=()
[[ $EUID -eq 0 ]] || as_root=(sudo -n)

for tool in sqlite3 git; do
    command -v "$tool" >/dev/null || {
        echo "$0: $tool is required on PATH" >&2
        exit 1
    }
done

# Build first, as the invoking user: the binaries are all this machine runs.
declare -a binaries=()
for target in $targets; do
    package=${target%%:*}
    test=${target#*:}
    executable=$(
        cd "$repo" && "$cargo" test --locked -p "$package" --test "$test" --no-run \
            --message-format=json |
            python3 -c '
import json, sys
test = sys.argv[1]
for line in sys.stdin:
    try:
        message = json.loads(line)
    except ValueError:
        continue
    target = message.get("target") or {}
    if (message.get("reason") == "compiler-artifact" and message.get("executable")
            and target.get("name") == test and "test" in target.get("kind", [])):
        print(message["executable"])
' "$test"
    )
    [[ -x $executable ]] || {
        echo "$0: no test binary for $package --test $test" >&2
        exit 1
    }
    binaries+=("$executable")
done

stores=$work/stores
tests=''
cleanup() {
    "${as_root[@]}" "$repo/tools/ci/native-stores.sh" down "$stores" || true
    [[ -z $tests ]] || "${as_root[@]}" rm -rf -- "$tests"
}
trap cleanup EXIT
mkdir -p "$work"
env_lines=$("${as_root[@]}" "$repo/tools/ci/native-stores.sh" up "$stores")
tests=$(mktemp -d /var/tmp/tilo-t1.XXXXXX)

status=0
for executable in "${binaries[@]}"; do
    echo "==> $(basename "$executable") $*"
    # shellcheck disable=SC2086 # env_lines is KEY=VALUE words without spaces
    "${as_root[@]}" env PATH="$PATH" TMPDIR="$tests" $env_lines \
        "$executable" --test-threads=1 "$@" || status=$?
done
exit "$status"
