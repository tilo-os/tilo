#!/usr/bin/env bash
# The nightly whole-workspace test run, in one of the two job sandboxes the
# `test` families give their packages (t.toml):
#
#   bash tools/ci/nightly-workspace-test.sh host       every package but those below
#   bash tools/ci/nightly-workspace-test.sh sandboxed  the packages listed in a test
#                                                      family's `nested_sandboxes`
#
# A package's tests pass in the sandbox its `test` family runs them in, and in no
# other: the host packages run as uid 0 in a root-owned namespace (runtime-fs's
# native fixtures and the imager's firstboot proofs check exactly that), while
# the sandboxed ones start namespaces of their own, which t allows only as
# uid 1000. So the nightly runs each half in a check of its own, with the
# sandbox its packages are tested in.
set -euo pipefail
part=${1:?usage: nightly-workspace-test.sh host|sandboxed}
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
cd "$root"

mapfile -t sandboxed < <(python3 tools/ci/sandboxed_test_packages.py)
selection=()
case $part in
    host) selection=(--workspace); for package in "${sandboxed[@]}"; do selection+=(--exclude "$package"); done ;;
    sandboxed) for package in "${sandboxed[@]}"; do selection+=(-p "$package"); done ;;
    *) echo "nightly-workspace-test.sh: unknown part $part" >&2; exit 2 ;;
esac

export TILO_TEST_ASSET_CACHE=$CARGO_TARGET_DIR/test-assets TILO_NTFS_SOURCE_CACHE=$CARGO_TARGET_DIR/ntfs-source
export TILO_IMAGER_TEST_CACHE_ROOT=$CARGO_TARGET_DIR/ci/imager-test-fs
mkdir -p "$TILO_IMAGER_TEST_CACHE_ROOT" && chmod 700 "$TILO_IMAGER_TEST_CACHE_ROOT"
cargo build --locked -p install --bin tilo-install
# The browser peer's suites drive Chromium and the MoQ library the browser runtime builds:
# the builder image's Chromium, which `CHROME` names, as in the `test` families (Chrome for
# Testing has no Linux arm64 build at all).
export CHROME="${CHROME:-/usr/lib64/chromium-browser/chromium-browser}"
bash tools/ci/ensure-browser-dependencies.sh
npm ci --prefix scripts/moq-browser --no-audit --no-fund
node scripts/moq-browser/build.mjs
# Where the kernel can enforce Landlock, the PDF tests require the helper's sandbox.
pdf_sandbox=$(bash tools/ci/require-pdf-sandbox.sh)
export TILO_REQUIRE_PDF_SANDBOX=$pdf_sandbox
filter=$(python3 tools/ci/nextest_filters.py | sed -n 's/^GOLDEN_FILTER=//p')
exec bash tools/test-env.sh cargo nextest run "${selection[@]}" --locked --profile ci -E "not ($filter)"
