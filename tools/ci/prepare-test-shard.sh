#!/usr/bin/env bash
# Prepare one check.yml test shard's environment from the tree under test.
#
# A pull request's workflow file comes from its head, but its tests come from
# the head merged onto main (.forgejo/actions/merge-main). A requirement added
# as a workflow step therefore never reaches a pull request branched before
# it, and that pull request fails main's tests for an environment it was never
# given. check.yml calls this script after the merge instead, so a requirement
# added here reaches every pull request whose workflow already calls it.
# Add each new shard requirement here, never as its own check.yml step
# (tools/tests/test_ci_test_shard_environment.py holds that line).
#
# Usage: prepare-test-shard.sh
#   TEST_PACKAGES  the shard's `-p NAME ...` arguments (may be empty)
#   SCOPED_TESTS   the shard's scoped reader tests as JSON (may be empty)
# Exports for the following steps go to $GITHUB_ENV.
set -euo pipefail

env_file=${GITHUB_ENV:?prepare-test-shard.sh writes the shard environment to GITHUB_ENV}

# Exact package names, so `-p notes` never matches some `-p notes-...`.
tested=" "
read -r -a arguments <<<"${TEST_PACKAGES:-}"
for (( index = 0; index < ${#arguments[@]}; index++ )); do
    if [[ ${arguments[index]} == -p ]] && (( index + 1 < ${#arguments[@]} )); then
        tested+="${arguments[index + 1]} "
    fi
done
scoped=" $(python3 -c 'import json, sys
print(" ".join(entry["package"] for entry in json.loads(sys.argv[1] or "[]")))' "${SCOPED_TESTS:-}") "

tests() { # PACKAGE...: whether the shard runs any of these packages' tests
    local name
    for name; do [[ $tested == *" $name "* ]] && return 0; done
    return 1
}
reads() { # PACKAGE...: whether the shard runs these packages' tests or scoped tests
    local name
    tests "$@" && return 0
    for name; do [[ $scoped == *" $name "* ]] && return 0; done
    return 1
}

# The packages whose tests start sandboxes of their own (tools/ci/nested-sandboxes.toml) need
# a runner that lets them create user namespaces; say so where this one refuses them.
nested=()
# shellcheck disable=SC2086
[[ -z ${tested// /} ]] || mapfile -t nested < <(python3 tools/ci/nested_sandboxes.py $tested)
if (( ${#nested[@]} )) && ! bwrap --unshare-user --ro-bind / / true 2>/dev/null; then
    echo "::warning::${nested[*]}: these tests start sandboxes of their own, and this runner refuses user namespaces (bwrap --unshare-user)"
fi

# Pinned/older builders may predate native PDF capture coverage. Stage only
# the verified runtime; leave Mesa, fonts and OS packages unchanged. Where the
# runner's kernel can enforce Landlock, the PDF helper's sandbox is also
# required rather than allowed to stand down.
if tests pdf-viewer tilo-pdf tilo-pdf-helper tilo-thumbnail; then
    asset=$(bash tools/ci/install-pdfium.sh --asset-name)
    ./os/dev/cache-github-assets.sh "$asset"
    library=$(bash tools/ci/install-pdfium.sh os/external-assets/cache /usr os/external-assets/github-assets.tsv)
    printf 'TILO_PDFIUM_LIB=%s\n' "$library" >> "$env_file"
    bash tools/ci/require-pdf-sandbox.sh "$env_file"
fi

# Imager deliberately rejects cache/source directories on the container
# overlay that also backs the operating-system root. The checked-out workspace
# is a host-backed qualified filesystem, so keep its native syscall fixtures
# there while retaining the container's ordinary /tmp behavior for every other
# package. Do not redirect HOME/TMPDIR/XDG_CACHE_HOME for the whole mixed
# package shard: those process-wide variables alter unrelated apps'
# persistence and git fixtures. Only the imager's native syscall tests consume
# this dedicated location.
if tests imager; then
    root="${GITHUB_WORKSPACE:-$PWD}/target/ci/imager-test-fs"
    mkdir -p "$root"
    chmod 700 "$root"
    bash tools/as-test-user.sh --own "$root"
    case "$(stat -f -c %T "$root")" in
        ext2/ext3|ext2/ext3/ext4|xfs|btrfs) ;;
        *) echo "workspace does not provide qualified imager semantics" >&2; exit 1 ;;
    esac
    printf 'TILO_IMAGER_TEST_CACHE_ROOT=%s\n' "$root" >> "$env_file"
fi

# app-cli registry and app-host local trust integration tests execute the
# installer; it is deliberately not a Rust dependency, so an affected-only
# Cargo invocation does not otherwise produce that binary.
if tests app-cli app-host; then
    bash tools/ci/measure.sh "build tilo-install test fixture" -- \
        bash scripts/ci-run.sh "cargo build (tilo-install test fixture)" -- \
        cargo build --locked -p install --bin tilo-install
fi
