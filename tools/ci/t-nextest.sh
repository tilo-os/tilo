#!/usr/bin/env bash
# The wrapper of t's `test` family (t.toml): runs the cargo nextest command t
# hands it as check.yml's test shards do.
#
#   t-nextest.sh cargo nextest run ... -p NAME ... [-E FILTER]
#
# It prepares what tools/ci/prepare-test-shard.sh prepares for the packages
# under test that a read-only image allows, then runs the command under
# tools/test-env.sh. Without an -E of its own, the run leaves out the visual
# goldens (the `goldens` check runs them alone, on their image) and the
# quarantined tests, as .config/nextest-filters.toml says. With one (`t test
# PKG --test NAME` asks for one test) the filter is left as given. Other cargo
# commands (`t cargo build` runs under this wrapper too) pass through unchanged.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

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
tests() { local name; for name; do [[ $tested == *" $name "* ]] && return 0; done; return 1; }

# Only the NTFS helper's own tests read the native images, which take minutes to build.
if tests tilo-ntfs-helper; then export TILO_TEST_NTFS_FIXTURES=1; else export TILO_TEST_NTFS_FIXTURES=0; fi

# The imager's native syscall tests need a cache on a qualified filesystem: the
# build directory, never the image's overlay.
if tests imager; then
    export TILO_IMAGER_TEST_CACHE_ROOT="${CARGO_TARGET_DIR:-$root/target}/ci/imager-test-fs"
    mkdir -p "$TILO_IMAGER_TEST_CACHE_ROOT"
    chmod 700 "$TILO_IMAGER_TEST_CACHE_ROOT"
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
    arguments+=(-E "not ($GOLDEN_FILTER) and not ($QUARANTINE_FILTER)")
fi
exec bash "$root/tools/test-env.sh" "${arguments[@]}"
