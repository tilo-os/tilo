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
# A `cargo test` also leaves out the tests whose capability this machine lacks, as a nextest run
# does (.config/nextest-capabilities.toml): tools/ci/capability_filter.py probes where the tests
# run and translates each gated group's filter into libtest skips, the runner hands the package's
# test binaries (tools/cargo-runner.sh), and the run says which tests it left out, why, and which
# check runs them.
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

# What a `-p` names: a package's name, or t's package id spec for a run that
# asks for a few packages (`path+file:///src/crates/desktop/settings#settings-store@0.1.0`).
package_name() {
    local spec=${1:-}
    spec=${spec##*#}
    printf '%s' "${spec%%@*}"
}

# The libtest skips that leave out the tests whose capability this machine
# lacks, as `.config/nextest-capabilities.toml` maps them: a `package<TAB>test`
# line per pattern, which tools/cargo-runner.sh hands that package's test
# binaries as `--skip`. A `cargo test` takes libtest's filters, not nextest's,
# so tools/ci/capability_filter.py translates each gated group's filter, from
# the probe it runs where the tests run, and the run says which tests it left
# out, why, and which check runs them. Every capability stays promised
# (tools/test-env-setup.sh sets its TILO_REQUIRE_*), so a test a group missed
# fails rather than skipping. A command that starts no test, or names no
# package the manifest has, leaves out nothing and probes nothing.
capability_skips() {
    local manifest="$repo/.config/nextest-capabilities.toml" argument name text
    local next=0 all=0 probes=0 index
    local -a packages=() arguments=()
    [[ ${1:-} == cargo && ${2:-} == test ]] || return 0
    [[ -r $manifest ]] || return 0
    # The probes are Linux's (tilo_test_env::PROBES): elsewhere a test that
    # needs one states so itself, and a probe that ran nowhere would refuse the
    # run instead of answering it.
    [[ $(uname -s) == Linux ]] || return 0
    arguments=("${@:3}")
    for ((index = 0; index < ${#arguments[@]}; index++)); do
        argument=${arguments[index]}
        if ((next)); then
            packages+=("$(package_name "$argument")")
            next=0
            continue
        fi
        case $argument in
            --) break ;;
            --no-run | --list | --doc | --bench | --benches) return 0 ;;
            --workspace | --all) all=1 ;;
            -p | --package) next=1 ;;
            -p?*) packages+=("$(package_name "${argument#-p}")") ;;
            --package=*) packages+=("$(package_name "${argument#--package=}")") ;;
        esac
    done
    # The manifest's own text says which packages it names: a run of none of
    # them probes nothing (the tool's answer), and asking would cost a build of
    # the probe for an answer it cannot use.
    if ((all == 0)) && ((${#packages[@]} > 0)); then
        text=$(<"$manifest")
        text=${text//[[:space:]]/}
        for name in "${packages[@]}"; do
            [[ $text == *"package($name)"* ]] && probes=1
        done
        ((probes)) || return 0
        python3 "$repo/tools/ci/capability_filter.py" --skip-patterns --packages "${packages[@]}"
    else
        python3 "$repo/tools/ci/capability_filter.py" --skip-patterns --packages
    fi
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
# The tests this machine cannot run, decided where they would run, before the
# steps that prepare the environment for the rest: a `cargo test` leaves them
# out through the runner (tools/cargo-runner.sh), as a nextest run does, and
# says so.
capability_skip_lines=
if [[ $mode == handoff ]]; then
    capability_skip_lines=$(capability_skips "$@")
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
    skips=$target/test-env-skips
    stage=$(mktemp -- "$target/.test-env-vars.XXXXXX")
    {
        printf 'pid=%s\n' $$
        cat -- "$env_file"
    } >"$stage"
    mv -- "$stage" "$vars"
    rm -f -- "$env_file" "$skips"
    # The same for the tests to leave out, beside the environment the runner
    # hands the tests that do run.
    if [[ -n $capability_skip_lines ]]; then
        stage=$(mktemp -- "$target/.test-env-skips.XXXXXX")
        {
            printf 'pid=%s\n' $$
            printf '%s\n' "$capability_skip_lines"
        } >"$stage"
        mv -- "$stage" "$skips"
    fi
    trap 'rm -f -- "$vars" "$skips"' EXIT
    if "$@"; then status=0; else status=$?; fi
    exit "$status"
fi

while IFS= read -r line; do
    export "${line%%=*}=${line#*=}"
done <"$env_file"
rm -f -- "$env_file"

# The fixture cache lives under the caller's HOME; the command gets a fresh one.
exec bash "$repo/tools/with-test-environment.sh" "$@"
