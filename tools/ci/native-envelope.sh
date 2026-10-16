#!/usr/bin/env bash
# Run a build script in the project's builder image under a fixed native
# envelope (MEMORY_GIB of memory, no swap, 24 CPUs), and fail unless
# tools/ci/qualify_native_envelope.py proves it stayed inside: no memory
# limit events, no OOM kill and a peak below the limit.
#
#   native-envelope.sh MEMORY_GIB SCRIPT
#
# For t's privileged checks (`sdk-envelope`, `web-size-envelope-bundle`): the
# node is a VM of its own, so the container's cgroup holds exactly this build.
# The sources are mounted at /src, and the build directory is /src/target.
set -euo pipefail
memory=${1:?usage: native-envelope.sh MEMORY_GIB SCRIPT}
script=${2:?usage: native-envelope.sh MEMORY_GIB SCRIPT}
case $memory in 22|36) ;; *) echo "unsupported envelope: ${memory} GiB" >&2; exit 2 ;; esac
image=$(sed -n 's/^image = "\(.*\)"$/\1/p' t.toml)
image=${image%%$'\n'*}
[[ $image == *@sha256:* ]] || { echo "t.toml names no pinned project image" >&2; exit 2; }
# The node's storage takes neither overlay nor a CNI network database.
exec podman --storage-driver vfs run --rm --network host \
    --memory "${memory}g" --memory-swap "${memory}g" --cpus 24 \
    -v "$PWD:/src" -w /src \
    -e MEMORY_GIB="$memory" -e CARGO_TARGET_DIR=/src/target -e CARGO_INCREMENTAL=0 \
    -e CARGO_PROFILE_DEV_DEBUG=0 -e CARGO_PROFILE_TEST_DEBUG=0 -e CARGO_TERM_COLOR=never \
    -e SCCACHE_IGNORE_SERVER_IO_ERROR=1 -e WASM_BINDGEN_VERSION=0.2.108 \
    "$image" bash -euo pipefail -c '
state=$(mktemp)
envelope() { python3 tools/ci/qualify_native_envelope.py "$1" --memory-gib "$MEMORY_GIB" --cpus 24 --state "$state"; }
envelope prepare
envelope before
status=0
bash "$1" || status=$?
envelope after || status=1
exit "$status"
' _ "$script"
