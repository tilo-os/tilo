#!/usr/bin/env bash
# Run workspace tests locally the way CI runs them.
#
#   scripts/test-local.sh                            # every workspace test but the goldens
#   scripts/test-local.sh -p tilo-fs -p tilo-ui           # selected packages
#   scripts/test-local.sh -p tilo-fs -- checked_restore   # ... and only matching tests
#   scripts/test-local.sh --goldens                  # only the visual goldens
#
# Arguments before `--` select what to build (packages, features); arguments
# after it go to `nextest run` (test-name filters, --no-capture, ...). Tests
# run with the `ci` nextest profile (one retry, JUnit report in
# target/nextest/ci/junit.xml) in a cleared environment built by
# tools/with-test-environment.sh: LANG=C.UTF-8, TZ=UTC, fresh HOME/XDG
# directories and no desktop session. GPU tests use Mesa's llvmpipe software
# Vulkan driver like CI, and audio tests an isolated PulseAudio endpoint when
# one can be started.
#
# Compilation runs first, in your own environment, so a compiler cache, a
# build container or a remote builder keeps working
# (CARGO=my-cargo scripts/test-local.sh -p tilo-fs). The test run then reuses those
# binaries through nextest's build metadata and never invokes Cargo.
set -euo pipefail

repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo"

goldens=0
build_args=()
run_args=()
while [ "$#" -gt 0 ]; do
    case "$1" in
        --goldens) goldens=1 ;;
        --) shift; run_args=("$@"); break ;;
        *) build_args+=("$1") ;;
    esac
    shift
done
[ "${#build_args[@]}" -gt 0 ] || build_args=(--workspace)

# CI keeps the visual goldens out of the parallel run and verifies them on an
# otherwise idle software renderer. The nextest gpu-golden group is the one
# definition of that set.
golden_filter=$(python3 - <<'PY'
import tomllib
with open(".config/nextest.toml", "rb") as handle:
    config = tomllib.load(handle)
filters = [override["filter"] for override in config["profile"]["ci"]["overrides"]
           if override.get("test-group") == "gpu-golden"]
if len(filters) != 1:
    raise SystemExit("expected exactly one gpu-golden override in .config/nextest.toml")
print(filters[0])
PY
)
if [ "$goldens" -eq 1 ]; then
    expression="$golden_filter"
else
    expression="not ($golden_filter)"
fi

cargo=${CARGO:-cargo}
nextest=$(command -v cargo-nextest) || {
    echo "cargo-nextest is required: cargo install --locked cargo-nextest" >&2
    exit 1
}
metadata="$repo/target/nextest/local"
mkdir -p "$metadata"
"$cargo" nextest list --locked --profile ci --list-type binaries-only \
    --message-format json "${build_args[@]}" >"$metadata/binaries.json"
"$cargo" metadata --locked --format-version 1 --all-features --no-deps \
    >"$metadata/cargo.json"

# Like a CI lane, require what this machine provides: a test that then
# finds no adapter or audio endpoint fails rather than skipping.
required=()
driver="/usr/share/vulkan/icd.d/lvp_icd.$(uname -m).json"
if [ -r "$driver" ]; then
    required+=(TILO_REQUIRE_GPU=1)
else
    echo "note: $driver is missing; install mesa-vulkan-drivers or GPU tests skip" >&2
fi

runner=(bash tools/with-test-environment.sh)
if command -v pulseaudio >/dev/null 2>&1 \
    && bash tools/with-headless-audio.sh true >/dev/null 2>&1; then
    runner+=(bash tools/with-headless-audio.sh)
    required+=(TILO_REQUIRE_AUDIO=1)
else
    echo "note: no isolated pulseaudio endpoint (tools/with-headless-audio.sh); audio tests skip" >&2
fi

# Start from nothing, as a CI container does, and pass through only what
# locates the toolchain. Tests that run cargo or rustc themselves get the
# rustup proxies, which work under the temporary HOME.
cargo_home=${CARGO_HOME:-$HOME/.cargo}
clean=(
    "PATH=$cargo_home/bin:$PATH"
    "HOME=$HOME"
    "CARGO_HOME=$cargo_home"
    "RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}"
    "TERM=${TERM:-dumb}"
    "WGPU_BACKEND=vulkan"
    "VK_DRIVER_FILES=$driver"
    "WGPU_ADAPTER_NAME=llvmpipe"
    "${required[@]}"
)
for name in USER LOGNAME CARGO_TERM_COLOR TILO_PDFIUM_LIB NEXTEST_TEST_THREADS; do
    if [ -n "${!name:-}" ]; then
        clean+=("$name=${!name}")
    fi
done

exec env -i "${clean[@]}" "${runner[@]}" \
    "$nextest" nextest run --profile ci \
    --binaries-metadata "$metadata/binaries.json" --cargo-metadata "$metadata/cargo.json" \
    -E "$expression" "${run_args[@]}"
