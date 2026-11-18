#!/usr/bin/env bash
# Give COMMAND the environment CI's test jobs give the suite.
#
#   tools/test-env.sh bash tools/ci/example-scenarios.sh -p example-counter
#
# The environment (tools/test-env-setup.sh's steps) reaches the test processes, never a build:
# a variable in a build's environment keys each build script's and proc macro's cache entry by
# it, and these differ per job (a private audio endpoint, a fresh HOME) and per package, so
# builds that carry them share no cache.
#
# - `cargo nextest` prepares the environment itself, where its tests execute
#   (.config/nextest.toml's ci profile), so such a command passes unchanged;
# - `cargo test` or `cargo bench` hands its tests to cargo's runner (tools/cargo-runner.sh), so
#   the command gets the caller's environment and the steps' variables go beside the build
#   directory, where the runner hands them, with the isolation below, to each test process;
# - any other command is itself the test (a script, an example scenario, a test binary started
#   by hand, a nextest command reading an archive, whose test binaries no runner starts), so it
#   gets the environment: the steps' variables and tools/with-test-environment.sh (LANG=C.UTF-8,
#   TZ=UTC, a fresh HOME and XDG config/data/state/cache/runtime tree removed afterwards, and no
#   desktop session: DISPLAY, WAYLAND_*, the session bus and keyring sockets removed).
#
# It provides:
# - tools/test-env-setup.sh's runtime, fonts and audio steps: lavapipe as the
#   only Vulkan adapter, PDFium, the shared fixture cache (TILO_TEST_CACHE_DIR),
#   the capabilities the image promises (TILO_REQUIRE_*), the pinned font
#   catalog and a private software audio endpoint that lives as long as
#   COMMAND. A nextest run prepares the last two only for the packages that
#   can use them; a command here may be anything, so it gets them all;
# - its ntfs step (the native NTFS images) unless TILO_TEST_NTFS_FIXTURES=0.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 COMMAND [ARG ...]" >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

# The pinned font catalog tilo-font-catalog's build script prepares, cached in the build directory
# (as tools/ci/nextest.sh gives a nextest command), so a warm build directory fetches it once. A
# build input, not a test one, and the same value in every job of a suite: every mode below gets it.
export TILO_FONT_ASSET_CACHE="${TILO_FONT_ASSET_CACHE:-${CARGO_TARGET_DIR:-$repo/target}/font-artifacts}"

# What the command does with the test environment: hand it over to the tests cargo starts, leave
# it to a nextest command, or be the test itself.
mode=environment
if [[ ${1:-} == cargo ]]; then
    case ${2:-} in
        test | bench) mode=handoff ;;
        nextest) mode=nextest ;;
    esac
fi
# A nextest command reading an archive executes its test binaries itself: it is the test.
if [[ $mode == nextest ]]; then
    for argument in "$@"; do
        case $argument in
            --archive-file | --archive-file=*) mode=environment ;;
        esac
    done
    if [[ $mode == nextest ]]; then
        exec "$@"
    fi
fi

steps=(runtime fonts audio)
[[ ${TILO_TEST_NTFS_FIXTURES:-1} == 0 ]] || steps+=(ntfs)
env_file=$(mktemp)
# The audio endpoint lives as long as this process, which becomes the command's (environment
# below) or waits for it (handoff), so it ends with COMMAND.
if ! NEXTEST_ENV=$env_file TILO_TEST_ENV_OWNER=$$ bash "$repo/tools/test-env-setup.sh" "${steps[@]}"; then
    rm -f -- "$env_file"
    exit 1
fi

if [[ $mode == handoff ]]; then
    # Where tools/cargo-runner.sh reads it: the build directory the tests' binaries are in. Its
    # first line names this command, so the runner passes over a file a killed job left behind.
    target=${CARGO_TARGET_DIR:-$repo/target}
    mkdir -p -- "$target"
    vars=$target/test-env-vars
    stage=$(mktemp -- "$target/.test-env-vars.XXXXXX")
    {
        printf 'pid=%s\n' $$
        cat -- "$env_file"
    } >"$stage"
    mv -- "$stage" "$vars"
    rm -f -- "$env_file"
    trap 'rm -f -- "$vars"' EXIT
    if "$@"; then status=0; else status=$?; fi
    exit "$status"
fi

while IFS= read -r line; do
    export "${line%%=*}=${line#*=}"
done <"$env_file"
rm -f -- "$env_file"

# The fixture cache lives under the caller's HOME; the command gets a fresh one.
exec bash "$repo/tools/with-test-environment.sh" "$@"
