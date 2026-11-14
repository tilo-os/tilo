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
# as .config/nextest-filters.toml says. With one the filter is left as given,
# but for the tests whose capability this machine lacks
# (.config/nextest-capabilities.toml): those are left out of every run, the
# probe of them taken here, where the tests run, so a workflow's run and a job
# t shards leave them out alike.
# Other cargo commands pass through unchanged.
#
# A whole run records each package's test time (tools/ci/ci_test_timings.py): it
# asks libtest for its own JSON events (--message-format, which a test binary
# takes beside the filters) and passes every line on to the job's log unchanged,
# recording each test's name and time as it goes, and adds what this job
# recorded to the run's records in the build directory
# (docs/measurements/test-timings.json). It never decides the run: the tests'
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
# What a `-p`/`--package` value names: a package's name, or t's package id spec for a run that
# asks for a few packages (`path+file:///src/crates/system/settings-store#settings-store@0.1.0`).
package_name() {
    local spec=${1:-}
    spec=${spec##*#}
    printf '%s' "${spec%%@*}"
}
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        -p|--package) tested+="$(package_name "${arguments[index + 1]}") " ;;
        -E|--filterset|--filter-expr) explicit=1 ;;
    esac
done
# Whether this run tests one of the named packages. A run that shards the whole family names
# no `-p` and is handed its shard's packages instead (T_NEXTEST_PACKAGES, as t shards a family),
# so a package the wrapper has to prepare before the tests run is found either way.
tests() {
    local name
    for name; do
        [[ $tested == *" $name "* ]] && return 0
        [[ " ${T_NEXTEST_PACKAGES:-} " == *" $name "* ]] && return 0
    done
    return 1
}

# In CI, a build of named packages resolves every member's features as the whole workspace does,
# its tests' features included (tools/ci/workspace_unify.py), so jobs testing different packages
# share each member's unit, and a test target a feature gates is a target of the run. That build
# compiles every member, so outside CI (a developer's `cargo test -p NAME` on one incremental
# target) an edit would rebuild every package that depends on the edited one; there the named
# packages build alone.
unify=()
[[ -n ${CI:-} ]] && unify=(-p workspace-unify)

# A run that names no `-p` gets no features here: t builds a family without a unify member whole,
# and that family's own t.toml `args` name the features its packages' tests need, so the command t
# resolves the family's unit graph from is the command that runs.
if (( nextest )) && [[ " $* " == *" -p "* || " $* " == *" --package "* ]]; then
    # Not as --features: `tilo-media/decode` there also turns on the optional tilo-media of every
    # package selected beside it.
    arguments+=("${unify[@]}")
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
# A run handed the program already (t builds it once in the family's build, t.toml's `starts`, and
# names it to every shard as NEXTEST_BIN_EXE_<bin>) builds nothing: built here, every shard of the
# package compiled its ~1,030 units again.
if tests tilo-agent-participant; then
    handed=$(printenv NEXTEST_BIN_EXE_tilo-video-render || true)
    if [[ -n $handed && -x $handed ]]; then
        export TILO_VIDEO_RENDER=$handed
    else
        cargo build --locked -p tilo-video-render --bin tilo-video-render
        export TILO_VIDEO_RENDER="${CARGO_TARGET_DIR:-$root/target}/debug/tilo-video-render"
    fi
fi

# The app-cli and app-host trust tests execute the installer, which no package
# depends on. A run handed it already (t's `starts`, NEXTEST_BIN_EXE_tilo-install) has it where the
# build left it, beside the tests' own build, and builds nothing.
if tests app-cli app-host; then
    handed=$(printenv NEXTEST_BIN_EXE_tilo-install || true)
    [[ -n $handed && -x $handed ]] || cargo build --locked -p install --bin tilo-install
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
# A test whose capability the ci profile's setup promises (tools/test-env-setup.sh sets every
# mapped capability's TILO_REQUIRE_*) fails where this machine lacks it, so the run leaves that
# group out by the manifest's probe, taken here, where the tests run. It joins the filter the run
# already has (nextest takes one), so a job given none of its own keeps the goldens out too. Only
# a run that tests a package the manifest names probes: an ordinary job pays nothing for a probe
# whose leave-out it could not meet. A tree with no manifest
# (.config/nextest-capabilities.toml) maps no capability, so it leaves nothing out either.
if (( nextest )) && [[ ${3:-} == run && -f $root/.config/nextest-capabilities.toml ]]; then
    runtime_packages=$tested
    [[ $runtime_packages != " " ]] || runtime_packages=${T_NEXTEST_PACKAGES:-}
    # The tool's first answer, to a run of none of the manifest's packages, is that there is
    # nothing to probe. The manifest's own text says as much to this shell (a group names its
    # packages as `package(NAME)`), so the ordinary job starts no Python to hear it: a third of a
    # second of its one core, in each of a suite's thousand jobs. A run that names no package
    # tests them all, and asks.
    probes=1
    if [[ -n ${runtime_packages// /} ]]; then
        probes=0
        named=$(<"$root/.config/nextest-capabilities.toml")
        named=${named//[[:space:]]/}
        for name in $runtime_packages; do
            [[ $named != *"package($name)"* ]] || probes=1
        done
    fi
    left_out=
    if (( probes )); then
        # shellcheck disable=SC2086 # one package per word, as the runners name them
        left_out=$(python3 "$root/tools/ci/capability_filter.py" --packages $runtime_packages)
    fi
    if [[ -n $left_out ]]; then
        merged=0
        for (( index = 0; index < ${#arguments[@]}; index++ )); do
            case ${arguments[index]} in
                -E|--filterset|--filter-expr)
                    arguments[index + 1]="(${arguments[index + 1]}) & ($left_out)"
                    merged=1
                    ;;
            esac
        done
        (( merged )) || arguments+=(-E "$left_out")
    fi
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
# nextest's profile, given either as `--profile` or its short `-P`.
given_profile=0
for argument in "$@"; do
    case $argument in
        --profile|--profile=*|-P|-P=*) given_profile=1 ;;
    esac
done
if (( nextest )) && [[ ${3:-} == run ]] && (( ! given_profile )); then
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
        --no-fail-fast|--profile|-P) ;;
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
