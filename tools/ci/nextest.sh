#!/usr/bin/env bash
# The CI wrapper of test runs: runs the cargo nextest command it is given.
#
#   nextest.sh cargo nextest run ... -p NAME ... [-E FILTER]
#
# It prepares what the packages under test need beyond the builder image,
# then runs the command: a nextest run sets up its test environment itself
# (the ci profile's setup scripts, .config/nextest.toml), wherever cargo runs
# it; any other command that runs tests runs under tools/test-env.sh. A command
# that only builds (t's listing of a family's test binaries, and its builds of
# the programs those tests start) runs as given: it starts no test, so it
# neither waits for the test environment's steps nor carries their values
# into its units. What only a package's
# tests need (its native images, the PDF sandbox, the emulated TPM, a render
# helper, the PAM module) is a setup step of that profile too: in this
# wrapper's environment every one of them would key each build script's and
# proc macro's cache entry, so jobs testing different packages would share no
# build. Without an -E of its
# own, the run leaves out the visual goldens (they run alone, on their image) and
# the flows (their own check), as .config/nextest-filters.toml says. With one the
# filter is left as given, but for the tests whose capability this machine lacks
# (.config/nextest-capabilities.toml): those are left out of every run, the
# probe of them taken here, where the tests run, so a workflow's run and a job
# t shards leave them out alike.
# A run narrowed by `t test <check> --test <name>` is handed the names in T_TESTS,
# one per line, by the check that declares `tests = "env"` (t.toml); every nextest
# run it wraps is intersected with them (tools/ci/test_selection.py names them as
# t does: a test, or a module path for every test below it).
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
# Whether the command starts tests. t's build of a test family lists the test binaries (`cargo
# nextest list`, which builds them) and builds the programs their tests start (`cargo build`), both
# through this wrapper: neither starts a test, so neither needs a program built for one or the test
# environment.
runs_tests=1
if [[ ${1:-} == cargo ]]; then
    case ${2:-} in
        build | check | clippy | doc) runs_tests=0 ;;
        nextest) [[ ${3:-} == run ]] || runs_tests=0 ;;
    esac
fi
arguments=("$@")
# What follows `--` goes to the test binaries (`cargo test -p NAME -- FILTER`): every option this
# wrapper adds goes before it.
# Arrays that may be empty are expanded as ${a[@]+"${a[@]}"}: macOS's bash 3.2 takes an empty
# array for an unset one under `set -u`, and "${a[@]}" then stops the script.
tail=()
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    if [[ ${arguments[index]} == -- ]]; then
        tail=("${arguments[@]:index}")
        arguments=("${arguments[@]:0:index}")
        break
    fi
done
# What a `-p`/`--package` value names: a package's name, or t's package id spec for a run that
# asks for a few packages (`path+file:///src/crates/desktop/settings#settings-store@0.1.0`).
package_name() {
    local spec=${1:-}
    spec=${spec##*#}
    printf '%s' "${spec%%@*}"
}
named_features=0
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    case ${arguments[index]} in
        -p|--package) tested+="$(package_name "${arguments[index + 1]}") " ;;
        -E|--filterset|--filter-expr) explicit=1 ;;
        --features|--features=*|-F|-F?*|--all-features) named_features=1 ;;
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
# share each member's unit, and a test target a feature checks is a target of the run. That build
# compiles every member, so outside CI (a developer's `cargo test -p NAME` on one incremental
# target) an edit would rebuild every package that depends on the edited one; there the named
# packages build alone.
unify=()
[[ -n ${CI:-} ]] && unify=(-p workspace-unify)

# A run that names no `-p` gets no features here: t builds a family without a unify member whole,
# and that family's own t.toml `args` name the features its packages' tests need, so the command t
# resolves the family's unit graph from is the command that runs.
#
# A run that names features itself is left as given too. Their set is that command's decision,
# and the unify member's own manifest resolves the features of such a build differently: with
# `--all-features` it turns on its `never` feature, which builds its bin and with it the whole
# workspace, and the build gets none of the inputs t gives the packages that need them.
if (( nextest && ! named_features )) && [[ " $* " == *" -p "* || " $* " == *" --package "* ]]; then
    # Not as --features: `tilo-media/decode` there also turns on the optional tilo-media of every
    # package selected beside it.
    arguments+=(${unify[@]+"${unify[@]}"})
fi

# The programs the tests of these packages start, handed by t (tools/ci/handed.sh) or, for a
# developer's job, built here. Neither is a variable of the environment: only the programs the
# tests find where the build left them.
# shellcheck source=tools/ci/handed.sh
source "$root/tools/ci/handed.sh"

# The app-cli trust tests execute the installer, which no package depends on. A run
# handed it has it where the build left it, beside the tests' own build.
if (( runs_tests )) && tests app-cli; then
    path=$(handed_bin tilo-install)
    [[ -n $path ]] || cargo build --locked -p install --bin tilo-install
fi

# A task's programs run in the tilolang host's own program, which the session and an agent's
# API start beside themselves and no package depends on. A run handed it has it where the build
# left it, beside the tests' own build.
if (( runs_tests )) && tests tilo-agent-session tilo-session; then
    path=$(handed_bin tilolang-process)
    [[ -n $path ]] || cargo build --locked -p tilolang-process --bin tilolang-process
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
    left_out="not (${GOLDEN_FILTER:-})"
    # The flows (docs/architecture/flows.md) are the `flows` check's to run, tests whose name
    # carries a `flow_<name>` module: an ordinary run of their package leaves them out, so a flow
    # still being built stays red without turning the package's own test check red. A tree whose
    # shared filters know none (an older rev) leaves them in, as it did before they existed.
    [[ -z ${FLOWS_FILTER:-} ]] || left_out+=" & not (${FLOWS_FILTER})"
    # The budgets over optimised builds are the `app-open-budget` check's, which builds them.
    [[ -z ${BUDGETS_FILTER:-} ]] || left_out+=" & not (${BUDGETS_FILTER})"
    arguments+=(-E "$left_out")
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
# The tests a run names (`t test <check> --test`): a check whose t.toml declares `tests = "env"`
# is handed them in T_TESTS, one per line, and runs those alone. A command the check runs is
# narrowed by intersecting its own -E with the selection, so a command none of whose tests were
# named runs none and passes (--no-tests=pass, which the flows' checks ask for). The -E this adds
# is what keeps a narrowed run from recording test timings below: it names no whole package's
# tests.
if (( nextest )) && [[ ${3:-} == run ]] && [[ -n ${T_TESTS:-} ]]; then
    selection=$(python3 "$root/tools/ci/test_selection.py")
    merged=0
    for (( index = 0; index < ${#arguments[@]}; index++ )); do
        case ${arguments[index]} in
            -E|--filterset|--filter-expr)
                arguments[index + 1]="(${arguments[index + 1]}) & ($selection)"
                merged=1
                ;;
        esac
    done
    (( merged )) || arguments+=(-E "$selection")
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
(( nextest || ! runs_tests )) || environment=(bash "$root/tools/test-env.sh")
if (( ! timed )); then
    exec ${environment[@]+"${environment[@]}"} "${arguments[@]}" ${tail[@]+"${tail[@]}"}
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
"${arguments[@]}" ${format[@]+"${format[@]}"} ${tail[@]+"${tail[@]}"} 2>&1 \
    | python3 -u "$root/tools/ci/ci_test_timings.py" record --json "$timings" || status=$?
# The run's own verdict, not the recording's.
exit "$status"
