#!/usr/bin/env bash
# Cargo's runner for Linux and macOS targets (.cargo/config.toml): `cargo test`,
# `cargo bench`, `cargo nextest` and `cargo run` start every binary through
# this script, locally and on a remote builder alike.
#
# Test and bench harnesses (target/<profile>/deps/*) and doctests always run
# inside tools/with-test-environment.sh: a fresh HOME and XDG tree and no
# desktop session. Whoever runs a test and however, it cannot reach their
# session bus, systemd user manager, logind, portals, media players, keyring
# or display. A job that is already inside that environment (CI,
# tools/test-env.sh for a command that is itself the test) keeps it.
#
# `cargo run` targets (target/<profile>/<bin>, examples) start unchanged:
# that is how a developer tries an app against their own desktop. The one
# exception is a command tools/test-env.sh wrapped: it prepares the test
# environment (a private audio endpoint, the fixtures, the capabilities the
# image promises) and writes it beside the build directory, so that a build's
# scripts never carry it. Every target such a command starts then gets it, and
# the isolation above, the way the test harnesses do. It also writes there the
# tests this machine cannot run, and a test binary of that package is started
# without them (`--skip`), as a nextest run leaves them out.
#
# A suite starts a process per test, so what a start costs here it pays tens
# of thousands of times. This shell is therefore the only one a start runs:
# it reads the script above into itself (`source`) rather than starting it,
# decides with the shell's own tests, and starts a program only for what
# changes the machine: the environment's directories and their removal
# (tools/tests/test_test_environment.py counts them).
set -euo pipefail
if [ "$#" -eq 0 ]; then
    echo "usage: $0 BINARY [ARG ...]" >&2
    exit 2
fi
case ${BASH_SOURCE[0]} in
    */*) tools=${BASH_SOURCE[0]%/*} ;;
    *) tools=. ;;
esac
# shellcheck source=tools/with-test-environment.sh
source "$tools/with-test-environment.sh"

harness=0
case $1 in
    */deps/* | */rustdoctest*/*) harness=1 ;;
esac

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

# The test environment tools/test-env.sh prepared, beside the build directory:
# its first line names the command that wrote it, so a file a killed command
# left behind is not read. A command whose tests get their environment from
# itself (a nextest command's ci profile, .config/nextest.toml) leaves none,
# and its `cargo run` targets start unchanged.
handoff=0
env_file=$dir/test-env-vars
if [ -r "$env_file" ]; then
    {
        IFS= read -r owner || true
        if [ "${owner%%=*}" = pid ] && kill -0 "${owner#pid=}" 2>/dev/null; then
            while IFS= read -r line; do
                [ -z "$line" ] || export "${line%%=*}=${line#*=}"
            done
            handoff=1
        fi
    } <"$env_file"
fi
# The tests this machine cannot run, which tools/test-env.sh left beside the
# environment: a test binary of a package they name skips them, as a nextest
# run leaves them out. Its first line names the command too.
args=("$@")
skip_file=$dir/test-env-skips
if [ "$harness" = 1 ] && [ "$handoff" = 1 ] && [ -r "$skip_file" ]; then
    {
        IFS= read -r owner || true
        if [ "${owner%%=*}" = pid ] && kill -0 "${owner#pid=}" 2>/dev/null; then
            while IFS='	' read -r package pattern; do
                [ -z "$pattern" ] || [ "$package" != "${CARGO_PKG_NAME:-}" ] ||
                    args+=("--skip" "$pattern")
            done
        fi
    } <"$skip_file"
fi
if [ "$handoff" = 0 ] && [ "$harness" = 0 ]; then
    exec "${args[@]}"
fi

if tilo_test_environment_intact; then
    exec "${args[@]}"
fi
tilo_test_environment_enter
# Started here rather than exec'd: this shell stays to remove the environment
# after it, as tools/with-test-environment.sh does.
if "${args[@]}"; then status=0; else status=$?; fi
exit "$status"
