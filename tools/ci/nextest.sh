#!/usr/bin/env bash
# The CI wrapper of test runs: runs the cargo nextest command it is given.
#
#   nextest.sh cargo nextest run ... -p NAME ... [-E FILTER]
#
# It prepares what the packages under test need beyond the builder image,
# then runs the command: a nextest run sets up its test environment itself
# (the ci profile's setup scripts, .config/nextest.toml), wherever cargo runs
# it; any other command runs under tools/test-env.sh. Without an -E of its
# own, the run leaves out the visual goldens (they run alone, on their image),
# as .config/nextest-filters.toml says. With one the filter is left as given.
# Other cargo commands pass through unchanged.
#
# A whole run records each package's test time (tools/ci/ci_test_timings.py): it
# asks libtest for its own JSON events (--message-format, which a test binary
# takes beside the filters) and passes every line on to the job's log unchanged,
# recording each test's name and time as it goes, and adds what this job
# recorded to the run's records in the build directory
# (docs/measurements/test-timings.md). It never decides the run: the tests'
# verdict is what nextest reported.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
target=${CARGO_TARGET_DIR:-$root/target}
timings=$target/test-timings.json

# The pinned fonts tilo-font-catalog's build script prepares, cached in the build directory, so
# a warm build directory fetches them once, not once per job.
export TILO_FONT_ASSET_CACHE="${CARGO_TARGET_DIR:-$root/target}/font-artifacts"

# The builder's Fedora RPM provides the real executable, not a mutable browser
# cache in HOME (test-env gives each run a fresh HOME). Keep explicit overrides
# and leave other platforms to their existing browser discovery.
if [[ $(uname -s) == Linux ]]; then
    export CHROME="${CHROME:-/usr/lib64/chromium-browser/chromium-browser}"
fi

# A crashing test leaves no core file: it lands in the package's directory, where a remote cargo
# drop-in copies it back into the worktree (hundreds of MB each); nextest's report already names
# the signal.
ulimit -c 0

tested=" "
explicit=0
nextest=0
[[ ${1:-} == cargo && ${2:-} == nextest ]] && nextest=1
arguments=("$@")
# What follows `--` goes to the test binaries (`cargo test -p NAME -- FILTER`): every option this
# wrapper adds goes before it.
tail=()
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    if [[ ${arguments[index]} == -- ]]; then
        tail=("${arguments[@]:index}")
        arguments=("${arguments[@]:0:index}")
        break
    fi
done
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        -p|--package) tested+="${arguments[index + 1]:-} " ;;
        -E|--filterset|--filter-expr) explicit=1 ;;
    esac
done
tests() { local name; for name; do [[ $tested == *" $name "* ]] && return 0; done; return 1; }

# In CI, a build of named packages resolves every member's features as the whole workspace does,
# its tests' features included (tools/ci/workspace_unify.py), so jobs testing different packages
# share each member's unit. That build compiles every member, so outside CI (a developer's
# `cargo test -p NAME` on one incremental target) an edit would rebuild every package that
# depends on the edited one; there the named packages build alone.
unify=()
[[ -n ${CI:-} ]] && unify=(-p workspace-unify)

if (( nextest )); then
    if [[ " $* " == *" -p "* || " $* " == *" --package "* ]]; then
        # Not as --features: `tilo-media/decode` there also turns on the optional tilo-media of
        # every package selected beside it.
        arguments+=("${unify[@]}")
    else
        # Package-qualified features: never enable inference for client-only apps.
        feature_flags=$(python3 "$root/tools/ci/package_features.py" "$@")
        read -r -a features <<<"$feature_flags"
        arguments+=("${features[@]}")
    fi
fi

# Only the NTFS helper's own tests read the native images, which take minutes to build.
if tests tilo-ntfs-helper; then export TILO_TEST_NTFS_FIXTURES=1; else export TILO_TEST_NTFS_FIXTURES=0; fi

# Where the node's kernel can enforce Landlock, the PDF tests require the helper's sandbox instead
# of letting an unconfined helper stand down (tools/ci/require-pdf-sandbox.sh).
if tests tilo-pdf pdf-viewer tilo-thumbnail; then
    pdf_sandbox=$(bash "$root/tools/ci/require-pdf-sandbox.sh")
    export TILO_REQUIRE_PDF_SANDBOX=$pdf_sandbox
fi

# An agent's host answers video renders in the `tilo-video-render` helper, which no
# package depends on; named explicitly, it is found also where an archive of
# test binaries runs away from the build directory.
if tests tilo-agent-participant; then
    cargo build --locked -p tilo-video-render --bin tilo-video-render
    export TILO_VIDEO_RENDER="${CARGO_TARGET_DIR:-$root/target}/debug/tilo-video-render"
fi

# The shell's sandbox and desktop tests drive a Wayland client from its own
# package, so a shell edit does not rebuild it; cargo only hands a test its own
# package's binaries. It resolves as the test build does (with workspace-unify in
# CI), so its units are the ones that build already made.
if (( nextest )) && tests shell; then
    cargo build --locked -p click-test-client "${unify[@]}" --bin click_test_client
    export TILO_CLICK_TEST_CLIENT="${CARGO_TARGET_DIR:-$root/target}/debug/click_test_client"
fi

# The app-cli and app-host trust tests execute the installer, which no package
# depends on.
if tests app-cli app-host; then
    cargo build --locked -p install --bin tilo-install
fi

# The sign-in and rollback tests observe an emulated TPM, which the builder has
# (dist/ci/install-build-deps.sh): one that cannot start it fails instead of
# passing by skipping.
if tests tilo-auth tilo-rollback tilo-test-env; then
    export TILO_REQUIRE_SWTPM=1
fi

# pam_tilo's tests load the module as libpam does: the cdylib, which no test
# binary links and an archive of test binaries does not carry.
if tests pam-tilo; then
    cargo build --locked -p pam-tilo --lib
    export TILO_PAM_TILO_MODULE="${CARGO_TARGET_DIR:-$root/target}/debug/libpam_tilo.so"
fi

if (( nextest && ! explicit )); then
    # shellcheck disable=SC1090
    source <(python3 - "$root/tools/ci" <<'PY'
import shlex, sys
sys.path.insert(0, sys.argv[1])
import nextest_filters
for name, value in nextest_filters.filters().items():
    print(f"{name}={shlex.quote(value)}")
PY
)
    arguments+=(-E "not (${GOLDEN_FILTER:-})")
fi
# A package whose sources declare no test at all (a load generator, a bin-only
# tool) passes with none run: run alone, nextest would answer "no tests to run",
# while batched with other packages it passes. Only such packages: a package
# with a test anywhere keeps nextest's verdict, and a build or listing failure
# fails either way.
declares_no_test() {
    [[ $tested != " " ]] || return 1
    python3 - "$root" $tested <<'PY'
import pathlib, re, sys, tomllib
root, names = pathlib.Path(sys.argv[1]), set(sys.argv[2:])
dirs = {}
for manifest in root.glob("**/Cargo.toml"):
    if "target" in manifest.parts or "vendor" in manifest.parts:
        continue
    try:
        name = tomllib.loads(manifest.read_text()).get("package", {}).get("name")
    except (OSError, tomllib.TOMLDecodeError):
        continue
    if name in names:
        dirs[name] = manifest.parent
test = re.compile(r"#\[([A-Za-z_]+::)*test\b")
sys.exit(0 if len(dirs) == len(names) and not any(
    test.search(path.read_text(errors="replace")) for d in dirs.values() for path in d.rglob("*.rs")) else 1)
PY
}
if (( nextest )) && [[ ${3:-} == run ]] && declares_no_test; then
    arguments+=(--no-tests=pass)
fi
if (( nextest )) && [[ ${3:-} == run ]] && [[ " $* " != *" --profile "* ]]; then
    # The profile CI tests with (`.config/nextest.toml`): the failure
    # output and the hang bound the job expects.
    arguments+=(--profile ci)
fi
# A run that stops at its first failure leaves the later packages out of the
# times, which would read as their tests getting faster; and a repeated run's
# passes and failures are nextest's measurement of a flaky test, not a timing.
timed=0
if (( nextest )) && [[ ${3:-} == run ]]; then
    timed=1
    [[ " $* " == *" --no-fail-fast "* ]] || arguments+=(--no-fail-fast)
fi
# Part of a package is not a package's time: a test filter given here selects by
# hand, and nextest's report is not a selection at all.
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        --no-fail-fast|--profile) ;;
        -E|--filterset|--filter-expr) timed=0 ;;
        --junit|--junit=*) [[ " $* " == *" --junit "xml" "* || " $* " == *" --junit=xml "* ]] || timed=0 ;;
        --test|--exact|--test-threads=*|--run-ignored=*|--retries=*) timed=0 ;;
    esac
done
# A name filter for the test binaries selects by hand too.
(( ${#tail[@]} > 1 )) && timed=0

environment=()
(( nextest )) || environment=(bash "$root/tools/test-env.sh")
if (( ! timed )); then
    exec "${environment[@]}" "${arguments[@]}" "${tail[@]}"
fi
# nextest's output and libtest's JSON events stay the log's own, as they were
# written: the events pass through unchanged, and the times
# are read from the same lines as they pass (ci_test_timings.py record). The job
# records into the run's file, which the run's other jobs add to, so the file
# holds the whole run.
# nextest takes one format: the run asks for libtest's events only when its caller did not.
format=(--message-format json-pretty)
[[ " $* " == *" --message-format "* || " $* " == *" --message-format="* ]] && format=()
status=0
"${arguments[@]}" "${format[@]}" "${tail[@]}" 2>&1 \
    | python3 -u "$root/tools/ci/ci_test_timings.py" record --json "$timings" || status=$?
# The run's own verdict, not the recording's.
exit "$status"
