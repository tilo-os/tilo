#!/usr/bin/env bash
# The wrapper of t's `test` family (t.toml): runs the cargo nextest command t
# hands it.
#
#   t-nextest.sh cargo nextest run ... -p NAME ... [-E FILTER]
#
# It prepares what the packages under test need beyond the builder image,
# then runs the command under
# tools/test-env.sh. Without an -E of its own, the run leaves out the visual
# goldens (the `goldens` check runs them alone, on its image), as
# .config/nextest-filters.toml says. With one (`t test
# PKG --test NAME` asks for one test) the filter is left as given. Other cargo
# commands (`t cargo build` runs under this wrapper too) pass through unchanged.
#
# t also runs the family from an archive of test binaries it built before, with
# no -p: T_NEXTEST_PACKAGES then names the packages. When only some test
# binaries can be affected by a change, T_NEXTEST_FILTER selects them, and every
# filter is narrowed to it.
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

# The pinned fonts tilo-font-catalog's build script prepares, cached in the build directory: t
# keeps that across a build lineage, so they are fetched once per lineage, not once per job.
export TILO_FONT_ASSET_CACHE="${CARGO_TARGET_DIR:-$root/target}/font-artifacts"

# The builder's Fedora RPM provides the real executable, not a mutable browser
# cache in HOME (test-env gives each run a fresh HOME). Keep explicit overrides
# and leave other platforms to their existing browser discovery.
if [[ $(uname -s) == Linux ]]; then
    export CHROME="${CHROME:-/usr/lib64/chromium-browser/chromium-browser}"
fi

tested=" "
explicit=0
nextest=0
[[ ${1:-} == cargo && ${2:-} == nextest ]] && nextest=1
arguments=("$@")
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        -p|--package) tested+="${arguments[index + 1]:-} " ;;
        -E|--filterset|--filter-expr) explicit=1 ;;
    esac
done
# t runs a family from an archive of its test binaries without -p, and names the packages here.
[[ $tested == " " && -n ${T_NEXTEST_PACKAGES:-} ]] && tested=" $T_NEXTEST_PACKAGES "
tests() { local name; for name; do [[ $tested == *" $name "* ]] && return 0; done; return 1; }

if (( nextest )); then
    if [[ " $* " == *" -p "* || " $* " == *" --package "* ]]; then
        # A build of named packages resolves every member's features as the whole workspace does,
        # its tests' features included (tools/ci/workspace_unify.py), so jobs testing different
        # packages share each member's unit. Not as --features too: `tilo-media/decode` there
        # also turns on the optional tilo-media of every package selected beside it.
        arguments+=(-p workspace-unify)
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
# package depends on; named explicitly, it is found also where t runs an archive of
# test binaries away from the build directory.
if tests tilo-agent-participant; then
    cargo build --locked -p tilo-video-render --bin tilo-video-render
    export TILO_VIDEO_RENDER="${CARGO_TARGET_DIR:-$root/target}/debug/tilo-video-render"
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
    arguments+=(-E "not (${GOLDEN_FILTER:-})${T_NEXTEST_FILTER:+ and ($T_NEXTEST_FILTER)}")
elif (( nextest )) && [[ -n ${T_NEXTEST_FILTER:-} ]]; then
    # t's selection of test binaries narrows the filters given.
    for (( index = 0; index < ${#arguments[@]}; index++ )); do
        case ${arguments[index]} in
            -E|--filterset|--filter-expr) arguments[index + 1]="(${arguments[index + 1]}) and ($T_NEXTEST_FILTER)" ;;
        esac
    done
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
    # The profile t's job is testing with (`.config/nextest.toml`): the failure
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
# hand, where the one t's job passes in T_NEXTEST_FILTER is the whole of what
# the job asked for, and nextest's report is not a selection at all.
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        --no-fail-fast|--profile) ;;
        -E|--filterset|--filter-expr) [[ -z ${T_NEXTEST_FILTER:-} ]] && timed=0 ;;
        --junit|--junit=*) [[ " $* " == *" --junit "xml" "* || " $* " == *" --junit=xml "* ]] || timed=0 ;;
        --test|--exact|--test-threads=*|--run-ignored=*|--retries=*) timed=0 ;;
    esac
done

if (( ! timed )); then
    exec bash "$root/tools/test-env.sh" "${arguments[@]}"
fi
# nextest's output and libtest's JSON events stay the log's own, as they were
# written: the events pass through as t's log already carries them, and the times
# are read from the same lines as they pass (ci_test_timings.py record). The job
# records into the run's file, which the run's other jobs add to, so the file
# holds the whole run.
status=0
bash "$root/tools/test-env.sh" "${arguments[@]}" --message-format json-pretty 2>&1 \
    | python3 -u "$root/tools/ci/ci_test_timings.py" record --json "$timings" || status=$?
# The run's own verdict, not the recording's.
exit "$status"
