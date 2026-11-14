#!/usr/bin/env bash
# Cargo's runner for Linux and macOS targets (.cargo/config.toml): `cargo test`,
# `cargo bench`, `cargo nextest` and `cargo run` start every binary through
# this script, locally and on a remote builder alike.
#
# Test and bench harnesses (target/<profile>/deps/*) and doctests always run
# inside tools/with-test-environment.sh: a fresh HOME and XDG tree and no
# desktop session. Whoever runs a test and however, it cannot reach their
# session bus, systemd user manager, logind, portals, media players, keyring
# or display. A run that is already inside that environment (CI,
# tools/test-env.sh) keeps it. Started as root in the
# CI image, they run as its test user (tools/as-test-user.sh).
#
# `cargo run` targets (target/<profile>/<bin>, examples) start unchanged:
# that is how a developer tries an app against their own desktop.
#
# A suite starts a process per test, so what a start costs here it pays tens
# of thousands of times. This shell is therefore the only one a start runs:
# it reads the two scripts above into itself (`source`) rather than starting
# them, decides with the shell's own tests, and starts a program only for
# what changes the machine: the environment's directories and their removal,
# and a handover where there is something to hand over
# (tools/tests/test_test_environment.py counts them).
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 BINARY [ARG ...]" >&2
    exit 2
fi
case $1 in
    */deps/* | */rustdoctest*/*) ;;
    *) exec "$@" ;;
esac

case ${BASH_SOURCE[0]} in
    */*) tools=${BASH_SOURCE[0]%/*} ;;
    *) tools=. ;;
esac
# shellcheck source=tools/with-test-environment.sh
source "$tools/with-test-environment.sh"

# The build directory: the nearest directory above the binary that holds
# cargo's .rustc_info.json (`dirname`, one level at a time).
dir=${1%/*}
while [ "$dir" != / ] && [ "$dir" != . ] && [ ! -e "$dir/.rustc_info.json" ]; do
    case $dir in
        */*) dir=${dir%/*} ;;
        *) dir=. ;;
    esac
    [ -n "$dir" ] || dir=/
done
# The build tree ($dir, which holds the profile directories) and the
# build directory's tmp (CARGO_TARGET_TMPDIR) that tests write into are
# the test user's too: cargo made them as whoever built. A test that
# writes beside the build's output — review images, fixture caches,
# prepared catalogs — lands in the tree, and root's mode 0755
# directories there would refuse it.
if [ -e "$dir/.rustc_info.json" ]; then
    tilo_test_user_own_tree "$dir"
fi
# The tmp goes over whenever it is root's (-O: this shell's user owns it, and a
# handover happens only as root), which is what cargo leaves after a build as
# root; one that is not root's is the test user's from an earlier start or
# from the job's walk, and starts no chown.
if [ -n "$tilo_test_user" ] && [ -d "$dir/tmp" ] && [ -O "$dir/tmp" ]; then
    tilo_test_user_own "$dir/tmp"
fi

if tilo_test_environment_intact; then
    tilo_test_user_exec "$@"
fi
tilo_test_environment_enter
# In a subshell that becomes the test: this shell stays to remove the
# environment after it.
if (tilo_test_user_exec "$@"); then status=0; else status=$?; fi
exit "$status"
