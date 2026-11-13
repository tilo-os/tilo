#!/usr/bin/env bash
# The test environment CI's test jobs give the suite, as setup steps that run
# where the tests run. nextest's `ci` profile runs them as its setup scripts
# (.config/nextest.toml), so `cargo nextest run --profile ci` sets the
# environment up on whichever machine executes the tests: a CI container, a
# developer machine or a remote builder cargo is forwarded to.
# tools/test-env.sh runs the same steps for commands that are not nextest.
#
#   tools/test-env-setup.sh STEP...
#
# Each step appends the variables it provides, as KEY=value lines, to the file
# named by NEXTEST_ENV (nextest hands them to the tests the step's rule
# selects). The image must provide the test runtime
# (dist/ci/install-build-deps.sh fedora test, or os/Containerfile.builder).
#
# Every job of a run pays for `runtime`, a job being one nextest run and a
# suite well over a thousand of them, so it names and promises and prepares
# nothing: the shell's own tests and reads, and no interpreter
# (tools/tests/test_test_environment.py holds it to that). What takes work is
# a step of its own, `fonts` and `audio`, which nextest runs only for the
# packages whose code can use it (.config/nextest.toml,
# tools/ci/setup_filters.py). `runtime` still promises both to every test
# (TILO_REQUIRE_FONTS, TILO_REQUIRE_AUDIO), so a test that needs one in a
# package the step leaves out fails, naming it, and never skips.
#
# runtime  Mesa's CPU Vulkan driver as the only adapter (VK_DRIVER_FILES,
#          WGPU_BACKEND, WGPU_ADAPTER_NAME), the renderer the visual goldens are
#          recorded with, and so TILO_REQUIRE_GPU=1; TILO_REQUIRE_MEDIA=1, as
#          the image carries the codecs; TILO_PDFIUM_LIB when the image
#          installed PDFium; and TILO_TEST_CACHE_DIR (default
#          ${XDG_CACHE_HOME:-$HOME/.cache}/tilo/test-env, the test user's
#          where tests run as one, tools/as-test-user.sh), so fixtures a test
#          generates itself (the media decode clip) are made once and shared
#          by every test process instead of once per process in its fresh HOME;
#          and TILO_REQUIRE_* of every capability .config/nextest-capabilities.toml
#          maps, whose tests the CI step leaves out where it is missing.
#          A TILO_REQUIRE_* or TILO_PDFIUM_LIB already set is kept, and a
#          machine that sets TILO_REQUIRE_GPU=0 (it promises no GPU) runs
#          without lavapipe rather than failing.
#          TILO_FONT_ASSET_CACHE (default $CARGO_TARGET_DIR/font-artifacts,
#          else the test cache's) names where the pinned font catalog is, and
#          TILO_REQUIRE_FONTS=1 says every frame is to be set in it: a frame
#          drawn in the host's faces is worse than a failure. TILO_REQUIRE_AUDIO=1
#          says the same of the audio endpoint.
# fonts    the pinned font catalog, prepared in TILO_FONT_ASSET_CACHE, where
#          the toolkit's catalog roots find it: a test job that reuses a build
#          from elsewhere carries no catalog of its own.
# audio    a private software audio endpoint (tools/with-headless-audio.sh),
#          and so TILO_REQUIRE_AUDIO=1. It lives as long as the process
#          TILO_TEST_ENV_OWNER names (default: the caller, which under nextest
#          is the test run) and is removed after it. An endpoint the
#          environment already has (an enclosing tools/test-env.sh) is kept;
#          with TILO_REQUIRE_AUDIO=0 (the machine promises no audio) there is
#          none.
# ntfs     TILO_NTFS_FIXTURES: the native NTFS images, generated once per
#          fixture generator revision and cached below TILO_TEST_CACHE_DIR.
#          An existing TILO_NTFS_FIXTURES is kept. nextest runs this step only
#          when the run selects a tilo-ntfs-helper test, as only those read
#          the images, which take minutes to build.
# root     TILO_TEST_AS_ROOT=1: the few tests that need root say so with this
#          step, and keep it where the others run as the test user
#          (tools/as-test-user.sh). In a container's own cgroup root, its
#          processes move to a leaf so the root delegates its controllers.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 runtime|fonts|audio|ntfs|root ..." >&2
    exit 2
fi
env_file=${NEXTEST_ENV:?test-env-setup.sh writes the environment to the file NEXTEST_ENV names}
case ${BASH_SOURCE[0]} in
    */*) repo=${BASH_SOURCE[0]%/*} ;;
    *) repo=. ;;
esac
repo=$(cd -- "$repo/.." && pwd)
# Who the tests run as, asked in this shell (tools/as-test-user.sh's
# functions) rather than of another one: a program starts only where there is
# a test user to look up.
# shellcheck source=tools/as-test-user.sh
source "$repo/tools/as-test-user.sh"
home=$HOME
[[ -z $tilo_test_user ]] || home=$(tilo_test_user_home)
if [[ $home == "$HOME" ]]; then
    cache_dir="${TILO_TEST_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tilo/test-env}"
else
    cache_dir="${TILO_TEST_CACHE_DIR:-$home/.cache/tilo/test-env}"
fi
# The build directory is the cache a job shares with the build; a run outside
# one keeps the catalog in the test cache, where it outlives the run rather
# than being written into the tree.
font_cache="${TILO_FONT_ASSET_CACHE:-${CARGO_TARGET_DIR:-$cache_dir}/font-artifacts}"

provide() { # KEY VALUE: the variable, for the tests and for the later steps
    printf '%s=%s\n' "$1" "$2" >>"$env_file"
    export "$1=$2"
}

# The fixture cache, the test user's: made where it is not there yet.
test_cache() {
    [[ -d $cache_dir ]] || (tilo_test_user_exec mkdir -p "$cache_dir")
}

# The TILO_REQUIRE_* variable of every capability
# .config/nextest-capabilities.toml marks `mapped`, into `mapped_variables`.
# tools/ci/capability_filter.py --require-variables prints the same list, and
# asking it cost every job of a run a Python start (a quarter of a second on a
# busy node, a quarter of this step) for two keys of a file this shell can
# read: a capability's own table, `[capabilities.NAME]`, with its `variable`
# and `mapped` written as keys on lines of their own.
# tools/tests/test_test_environment.py holds this reading to that tool's, on
# the manifest and on ones written to tell the two apart.
mapped_capabilities() {
    local manifest="$repo/.config/nextest-capabilities.toml" line table= variable= mapped=
    mapped_variables=()
    [[ -r $manifest ]] || return 0
    while IFS= read -r line || [[ -n $line ]]; do
        if [[ $line =~ ^[[:space:]]*\[ ]]; then
            if [[ -n $mapped && -n $variable ]]; then mapped_variables+=("$variable"); fi
            variable= mapped= table=
            if [[ $line =~ ^[[:space:]]*\[capabilities\.[A-Za-z0-9_-]+\][[:space:]]*(#.*)?$ ]]; then table=1; fi
        elif [[ -n $table ]]; then
            if [[ $line =~ ^[[:space:]]*variable[[:space:]]*=[[:space:]]*\"([A-Za-z0-9_]+)\"[[:space:]]*(#.*)?$ ]]; then
                variable=${BASH_REMATCH[1]}
            elif [[ $line =~ ^[[:space:]]*mapped[[:space:]]*=[[:space:]]*true[[:space:]]*(#.*)?$ ]]; then
                mapped=1
            fi
        fi
    done <"$manifest"
    if [[ -n $mapped && -n $variable ]]; then mapped_variables+=("$variable"); fi
}

runtime() {
    local icd required
    icd="/usr/share/vulkan/icd.d/lvp_icd.$(uname -m).json"
    if [[ -r $icd ]]; then
        provide WGPU_BACKEND vulkan
        provide WGPU_ADAPTER_NAME llvmpipe
        provide VK_DRIVER_FILES "$icd"
    elif [[ ${TILO_REQUIRE_GPU:-} != 0 ]]; then
        echo "test environment requires Mesa's lavapipe Vulkan driver ($icd)" >&2
        exit 1
    fi
    provide TILO_REQUIRE_GPU "${TILO_REQUIRE_GPU:-1}"
    provide TILO_REQUIRE_MEDIA "${TILO_REQUIRE_MEDIA:-1}"
    # Every capability .config/nextest-capabilities.toml maps is required: a
    # test that needs one fails where it is missing, unless the CI step left
    # it out after probing (tools/ci/capability_filter.py).
    mapped_capabilities
    for required in ${mapped_variables[@]+"${mapped_variables[@]}"}; do
        provide "$required" "${!required:-1}"
    done
    if [[ -n ${TILO_PDFIUM_LIB:-} ]]; then
        provide TILO_PDFIUM_LIB "$TILO_PDFIUM_LIB"
    elif [[ -r /usr/lib64/libpdfium.so ]]; then
        provide TILO_PDFIUM_LIB /usr/lib64/libpdfium.so
    fi
    test_cache
    provide TILO_TEST_CACHE_DIR "$cache_dir"
    # Where the pinned font catalog is, and the promise of it: a run promised the catalog stops
    # where one is missing, rather than comparing frames set in whatever faces the machine
    # happens to have. Preparing it is the `fonts` step's, for the packages that can set text; a
    # process of any other package that sets text all the same finds none and stops here.
    provide TILO_FONT_ASSET_CACHE "$font_cache"
    provide TILO_REQUIRE_FONTS "${TILO_REQUIRE_FONTS:-1}"
    # The audio endpoint is promised the same way, and is the `audio` step's to start: a test
    # that requires one (tilo_test_env's `Audio`) in a package that step leaves out fails.
    provide TILO_REQUIRE_AUDIO "${TILO_REQUIRE_AUDIO:-1}"
}

# The pinned font catalog every frame is set in. A build prepares its own into the cargo output
# directory of the crate that owns it, and that is not what a build directory carries from the
# job that built to the job that tests, so a test job that reuses a build elsewhere finds none
# and draws its frames in the host's faces: pixels that look almost right, judged against
# baselines recorded in the product's. Preparing it here, in the artifact cache the builder image
# seeds, puts it where the tests run and the toolkit's catalog roots read it: a job whose cache
# has none copies it from the image, and one that finds a catalog verifies what is there. A
# machine without the image's seed has the jobs that share the cache take turns (the cache's
# lock), so one fetches the catalog and the rest verify what it left rather than each fetching it.
# Copying and hashing its 54 MB is half a second of a job's one core, which only the packages
# that can read a catalog pay: those that depend on the crate that owns it
# (.config/nextest.toml's `rdeps(tilo-font-catalog)`).
fonts() {
    test_cache
    provide TILO_FONT_ASSET_CACHE "$font_cache"
    python3 "$repo/tools/font-catalog.py" root "$repo/crates/sdk/ui-core/assets/fonts" \
        --scope fixtures --cache "$font_cache" >/dev/null
}

audio() {
    local owner=${TILO_TEST_ENV_OWNER:-$PPID} ready daemon name line
    [[ ${TILO_REQUIRE_AUDIO:-} != 0 ]] || return 0
    if [[ -n ${TILO_HEADLESS_AUDIO_PID:-} && -n ${PULSE_RUNTIME_PATH:-} &&
        ${PULSE_SERVER:-} == "unix:$PULSE_RUNTIME_PATH/native" &&
        -S $PULSE_RUNTIME_PATH/native ]] && kill -0 "$TILO_HEADLESS_AUDIO_PID" 2>/dev/null; then
        for name in PULSE_RUNTIME_PATH PULSE_STATE_PATH PULSE_COOKIE PULSE_SERVER \
            TILO_HEADLESS_AUDIO_PID; do
            provide "$name" "${!name:-}"
        done
        provide TILO_REQUIRE_AUDIO "${TILO_REQUIRE_AUDIO:-1}"
        return
    fi
    kill -0 "$owner" 2>/dev/null || {
        echo "test-env: the audio endpoint's owner (pid $owner) is not running" >&2
        exit 1
    }
    # The endpoint is tools/with-headless-audio.sh's, running a command that
    # reports its environment once it is ready and then waits for the owner to
    # exit; the wrapper then stops the daemon and removes its runtime
    # directory. It runs in a session of its own with no descriptor of the
    # caller, so neither a test run's signals nor its output pipes reach it.
    ready=$(mktemp -d /tmp/tilo-audio-setup.XXXXXXXX)
    # shellcheck disable=SC2016
    setsid bash "$repo/tools/with-headless-audio.sh" bash -c '
        env | grep -E "^(PULSE_(RUNTIME_PATH|STATE_PATH|COOKIE|SERVER)|TILO_HEADLESS_AUDIO_PID|TILO_REQUIRE_AUDIO)=" >"$1.part"
        mv -- "$1.part" "$1"
        exec tail --pid="$2" -f /dev/null' audio-endpoint "$ready/env" "$owner" \
        </dev/null >"$ready/log" 2>&1 &
    daemon=$!
    # The wrapper bounds its own start (TILO_HEADLESS_AUDIO_TIMEOUT) and exits
    # when the daemon does not come up, which ends this wait too.
    while [[ ! -e $ready/env ]]; do
        if ! kill -0 "$daemon" 2>/dev/null; then
            cat "$ready/log" >&2
            rm -rf -- "$ready"
            echo "test-env: the headless audio endpoint did not start" >&2
            exit 1
        fi
        sleep 0.02
    done
    while IFS= read -r line; do
        provide "${line%%=*}" "${line#*=}"
    done <"$ready/env"
    rm -rf -- "$ready"
}

ntfs() {
    local crate="$repo/crates/session/tilo-ntfs-helper"
    local key root dir stage status=0
    if [[ -n ${TILO_NTFS_FIXTURES:-} ]]; then
        provide TILO_NTFS_FIXTURES "$TILO_NTFS_FIXTURES"
        return
    fi
    # The generator, the build it drives (which pins the vendored source by
    # digest) and the C fixture writer determine the images.
    key=$(cat "$crate/tests/build_native_fixtures.py" "$crate/build_native.py" \
        "$crate/source_artifact.py" "$crate/tests/fixture_builder.c" | sha256sum | cut -c1-16)
    root="$cache_dir/ntfs-fixtures"
    dir="$root/$key"
    mkdir -p "$root"
    exec 9>"$root/.lock"
    flock 9
    if [[ ! -e $dir/manifest.json ]]; then
        echo "test-env: generating native NTFS fixtures in $dir" >&2
        stage=$(mktemp -d "$root/.stage.XXXXXX")
        if python3 "$crate/tests/build_native_fixtures.py" \
            --output "$stage/fixtures" --tools-output "$stage/tools" \
            >"$stage/log" 2>&1; then
            rm -rf -- "$dir"
            mv -- "$stage/fixtures" "$dir"
        else
            status=$?
            cat "$stage/log" >&2
        fi
        rm -rf -- "$stage"
        if [[ $status -ne 0 ]]; then
            echo "test-env: native NTFS fixture generation failed" >&2
            exit "$status"
        fi
    fi
    exec 9>&-
    provide TILO_NTFS_FIXTURES "$dir"
}

root() {
    provide TILO_TEST_AS_ROOT 1
    delegate_cgroups
}

# The limits tests give jobs cgroups of their own, under one that hands its
# children the controllers. A cgroup holding processes cannot: a container's
# own root, where every process of the run starts, is one. As root there, its
# processes move into a leaf and the root hands its children every controller
# it is given, as a container that starts containers does.
delegate_cgroups() {
    local cg=/sys/fs/cgroup pids pid controller
    [[ $(id -u) == 0 && $(cat /proc/self/cgroup) == 0::/ && -w $cg/cgroup.subtree_control ]] || return 0
    [[ -z $(cat "$cg/cgroup.subtree_control") ]] || return 0
    mkdir -p "$cg/init"
    # Again while any is left: a process may fork while the others move.
    for _ in 1 2 3 4 5; do
        pids=$(cat "$cg/cgroup.procs")
        [[ -n $pids ]] || break
        for pid in $pids; do
            echo "$pid" >"$cg/init/cgroup.procs" 2>/dev/null || true
        done
    done
    for controller in $(cat "$cg/cgroup.controllers"); do
        echo "+$controller" >"$cg/cgroup.subtree_control" ||
            echo "the cgroup root does not delegate $controller" >&2
    done
}

for step; do
    case $step in
        runtime | fonts | audio | ntfs | root) "$step" ;;
        *) echo "unknown test environment step: $step" >&2; exit 2 ;;
    esac
done
