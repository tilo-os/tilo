#!/usr/bin/env bash
# Run COMMAND in the environment CI's test jobs give the suite, so the whole
# workspace suite behaves the same in CI, in the developer distrobox and on a
# remote builder. The image must provide the test runtime
# (dist/ci/install-build-deps.sh fedora test, or os/Containerfile.builder).
#
#   tools/test-env.sh cargo nextest run --workspace --locked --profile ci \
#       -E 'not (...visual goldens...)'
#
# It provides:
# - tools/with-test-environment.sh: LANG=C.UTF-8, TZ=UTC, a fresh HOME and XDG
#   config/data/state/cache/runtime tree removed afterwards, and no desktop
#   session (DISPLAY, WAYLAND_*, the session bus and keyring sockets removed);
# - Mesa's CPU Vulkan driver as the only adapter (VK_DRIVER_FILES, WGPU_BACKEND,
#   WGPU_ADAPTER_NAME), the renderer the visual goldens are recorded with;
# - TILO_PDFIUM_LIB when the image installed PDFium and it is not already set;
# - TILO_NTFS_FIXTURES: the native NTFS images, generated once per fixture
#   generator revision and cached below TILO_TEST_CACHE_DIR (default
#   ${XDG_CACHE_HOME:-$HOME/.cache}/tilo/test-env). An existing
#   TILO_NTFS_FIXTURES is kept; TILO_TEST_NTFS_FIXTURES=0 skips generating them
#   when the selected tests do not need them;
# - TILO_TEST_CACHE_DIR, exported, so fixtures a test generates itself (the
#   media decode clip) are encoded once and shared by every test process of
#   the run instead of once per process in its fresh HOME;
# - a private software audio endpoint, via tools/with-headless-audio.sh.
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 COMMAND [ARG ...]" >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
export TILO_TEST_CACHE_DIR="${TILO_TEST_CACHE_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/tilo/test-env}"
mkdir -p "$TILO_TEST_CACHE_DIR"

unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET DBUS_SESSION_BUS_ADDRESS

export WGPU_BACKEND=vulkan
export WGPU_ADAPTER_NAME=llvmpipe
export VK_DRIVER_FILES="/usr/share/vulkan/icd.d/lvp_icd.$(uname -m).json"
[[ -r $VK_DRIVER_FILES ]] || {
    echo "test environment requires Mesa's lavapipe Vulkan driver ($VK_DRIVER_FILES)" >&2
    exit 1
}

if [[ -z ${TILO_PDFIUM_LIB:-} && -r /usr/lib64/libpdfium.so ]]; then
    export TILO_PDFIUM_LIB=/usr/lib64/libpdfium.so
fi

ntfs_fixtures() {
    local crate="$repo/crates/session/tilo-ntfs-helper"
    local key root dir stage status=0
    # The generator, the build it drives (which pins the vendored source by
    # digest) and the C fixture writer determine the images.
    key=$(cat "$crate/tests/build_native_fixtures.py" "$crate/build_native.py" \
        "$crate/tests/fixture_builder.c" | sha256sum | cut -c1-16)
    root="$TILO_TEST_CACHE_DIR/ntfs-fixtures"
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
    export TILO_NTFS_FIXTURES="$dir"
}
if [[ -z ${TILO_NTFS_FIXTURES:-} && ${TILO_TEST_NTFS_FIXTURES:-1} != 0 ]]; then
    ntfs_fixtures
fi

# The fixture cache lives under the caller's HOME; the tests themselves get a
# fresh one.
exec bash "$repo/tools/with-test-environment.sh" \
    bash "$repo/tools/with-headless-audio.sh" "$@"
