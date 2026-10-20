#!/usr/bin/env bash
# The wrapper of t's `test` family (t.toml): runs the cargo nextest command t
# hands it.
#
#   t-nextest.sh cargo nextest run ... -p NAME ... [-E FILTER]
#
# It prepares what the packages under test need beyond the builder image,
# then runs the command under
# tools/test-env.sh. Without an -E of its own, the run leaves out the visual
# goldens (the `goldens` check runs them alone, on their image) and the
# quarantined tests, as .config/nextest-filters.toml says. With one (`t test
# PKG --test NAME` asks for one test) the filter is left as given. Other cargo
# commands (`t cargo build` runs under this wrapper too) pass through unchanged.
#
# t also runs the family from an archive of test binaries it built before, with
# no -p: T_NEXTEST_PACKAGES then names the packages. When only some test
# binaries can be affected by a change, T_NEXTEST_FILTER selects them, and every
# filter is narrowed to it.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

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
    # Package-qualified features: never enable inference for client-only apps.
    feature_flags=$(python3 "$root/tools/ci/package_features.py" "$@")
    read -r -a features <<<"$feature_flags"
    arguments+=("${features[@]}")
fi

# Only the NTFS helper's own tests read the native images, which take minutes to build.
if tests tilo-ntfs-helper; then export TILO_TEST_NTFS_FIXTURES=1; else export TILO_TEST_NTFS_FIXTURES=0; fi

# The imager's native syscall tests need a cache on a qualified filesystem
# (ext4, XFS or Btrfs), never an overlay or tmpfs: the build directory where it
# is one, else the job's scratch TMPDIR (t mounts the build directory as an
# overlay when it stacks it on earlier builds).
if tests imager && [[ -z ${TILO_IMAGER_TEST_CACHE_ROOT:-} ]]; then
    qualified() { case $(stat -f -c %t "$1") in ef53|58465342|9123683e) return 0 ;; esac; return 1; }
    for candidate in "${CARGO_TARGET_DIR:-$root/target}/ci/imager-test-fs" "${TMPDIR:-/tmp}/imager-test-fs"; do
        mkdir -p "$candidate"
        chmod 700 "$candidate"
        if qualified "$candidate"; then
            export TILO_IMAGER_TEST_CACHE_ROOT=$candidate
            break
        fi
    done
    if [[ -z ${TILO_IMAGER_TEST_CACHE_ROOT:-} ]]; then
        echo "t-nextest.sh: neither the build directory nor TMPDIR is on ext4, XFS or Btrfs, which the imager's cache tests need" >&2
        exit 1
    fi
fi

# The app-cli and app-host trust tests execute the installer, which no package
# depends on.
if tests app-cli app-host; then
    cargo build --locked -p install --bin tilo-install
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
    arguments+=(-E "not ($GOLDEN_FILTER) and not ($QUARANTINE_FILTER)${T_NEXTEST_FILTER:+ and ($T_NEXTEST_FILTER)}")
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
exec bash "$root/tools/test-env.sh" "${arguments[@]}"
