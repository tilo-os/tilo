#!/usr/bin/env bash
# Give COMMAND the environment CI's test jobs give the suite.
#
#   tools/test-env.sh /target/debug/tilo-local headless --check --no-build --apps notes
#
# The environment (tools/test-env-setup.sh's steps) reaches the test processes, never a build:
# a variable in a build's environment keys each build script's and proc macro's cache entry by
# it, and these differ per job (a private audio endpoint, a fresh HOME) and per package, so
# builds that carry them share no cache.
#
# - `cargo nextest` prepares the environment itself, where its tests execute
#   (.config/nextest.toml's ci profile), so such a command passes unchanged;
# - `cargo test`, `cargo bench` or `cargo run` starts its programs in the environment through
#   cargo's runner (tools/cargo-runner.sh), so the command gets the caller's environment and the
#   steps' variables go beside the build directory, where the runner hands them, with the
#   isolation below, to each test, bench or target it starts;
# - any other command is itself the test (a script, an example scenario, a test binary started
#   by hand, a nextest command reading an archive, whose test binaries no runner starts), so it
#   gets the environment: the steps' variables and tools/with-test-environment.sh (LANG=C.UTF-8,
#   TZ=UTC, a fresh HOME and XDG config/data/state/cache/runtime tree removed afterwards, and no
#   desktop session: DISPLAY, WAYLAND_*, the session bus and keyring sockets removed);
# - `--build` runs a command in the caller's environment with the build inputs above and nothing
#   else, for a command that builds and then runs what it built: tools/ci/layout-check.sh and
#   tools/ci/example-scenarios.sh build that way and then run themselves under this script.
#
# A command started in the environment that builds the app it then starts finds the environment a
# build gets (`--build`'s: the caller's and the build inputs) beside the fresh HOME, so what it
# builds builds as a job's builds do, not in the test environment.
#
# It provides:
# - tools/test-env-setup.sh's runtime, fonts and audio steps: lavapipe as the
#   only Vulkan adapter, PDFium, the shared fixture cache (TILO_TEST_CACHE_DIR),
#   the capabilities the image promises (TILO_REQUIRE_*), the pinned font
#   catalog and a private software audio endpoint that lives as long as
#   COMMAND. A nextest run prepares the last two only for the packages that
#   can use them; a command here may be anything, so it gets them all;
# - its ntfs step (the native NTFS images) unless TILO_TEST_NTFS_FIXTURES=0 or a cargo command
#   names its packages without tilo-ntfs-helper.
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

# What the command does with the test environment: hand it over to the programs cargo starts,
# leave it to a nextest command, or be the test itself.
mode=environment
if [[ ${1:-} == cargo ]]; then
    case ${2:-} in
        test | bench | run) mode=handoff ;;
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

# The native NTFS images are read by tilo-ntfs-helper's tests alone, as nextest's registration
# says (.config/nextest.toml). A cargo command that names its packages without that one never
# reads them, so it isn't made to wait the minutes their generator takes; one naming none, or the
# whole workspace, and any other command, may.
ntfs_read() {
    [[ $mode == handoff ]] || return 0
    local argument named=0 next=0
    for argument in "${@:3}"; do
        if ((next)); then
            [[ $argument == tilo-ntfs-helper ]] && return 0
            next=0 named=1
            continue
        fi
        case $argument in
            --) break ;;
            --workspace | --all) return 0 ;;
            -p | --package) next=1 ;;
            -p* | --package=*)
                named=1
                [[ ${argument#-p} == tilo-ntfs-helper || ${argument#--package=} == tilo-ntfs-helper ]] && return 0
                ;;
        esac
    done
    ((named == 0))
}
steps=(runtime fonts audio)
if [[ ${TILO_TEST_NTFS_FIXTURES:-1} != 0 ]] && ntfs_read "$@"; then
    steps+=(ntfs)
fi
# The switch is this script's, not a build's or a test's: cargo's environment keys every build
# script's and proc macro's cache entry by it, so the command started in it must not carry it.
unset TILO_TEST_NTFS_FIXTURES
# A build: the caller's environment and the build inputs above, prepared by the same script every
# other build of the suite is given them by.
if [[ ${1:-} == --build ]]; then
    shift
    exec "$@"
fi
# A command started in the environment that builds the app it then starts (the app scenario
# runner's Cargo build) must build as --build builds: a variable in a build's environment keys
# every build script's and proc macro's cache entry by it, and the steps below differ per job.
# Write the environment this script was given, before the steps add theirs, to a file
# tools/with-test-environment.sh holds beside the fresh HOME, where it goes with the HOME.
if [[ $mode == environment && -z ${TILO_TEST_ENVIRONMENT:-} ]]; then
    build_environment=$(mktemp)
    env -0 >"$build_environment"
    TILO_BUILD_ENVIRONMENT_FILE=$build_environment
    export TILO_BUILD_ENVIRONMENT_FILE
fi
env_file=$(mktemp)
# The audio endpoint lives as long as this process, which becomes the command's (environment
# below) or waits for it (handoff), so it ends with COMMAND.
if ! NEXTEST_ENV=$env_file TILO_TEST_ENV_OWNER=$$ bash "$repo/tools/test-env-setup.sh" "${steps[@]}"; then
    rm -f -- "$env_file"
    [[ -z ${TILO_BUILD_ENVIRONMENT_FILE:-} ]] || rm -f -- "$TILO_BUILD_ENVIRONMENT_FILE"
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
