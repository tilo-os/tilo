#!/usr/bin/env bash
# Run COMMAND in the environment CI's test jobs give the suite, for commands
# that are not a nextest run: scripts, example scenarios, a test binary run by
# hand. `cargo nextest run --profile ci` sets the same environment up itself,
# where its tests execute (.config/nextest.toml runs tools/test-env-setup.sh),
# so it needs no wrapper; under this one it keeps the environment it is given.
#
#   tools/test-env.sh bash tools/ci/example-scenarios.sh -p example-counter
#
# It provides:
# - tools/test-env-setup.sh's runtime and audio steps: lavapipe as the only
#   Vulkan adapter, PDFium, the shared fixture cache (TILO_TEST_CACHE_DIR), the
#   capabilities the image promises (TILO_REQUIRE_*) and a private software
#   audio endpoint that lives as long as COMMAND;
# - its ntfs step (the native NTFS images) unless TILO_TEST_NTFS_FIXTURES=0;
# - tools/with-test-environment.sh: LANG=C.UTF-8, TZ=UTC, a fresh HOME and XDG
#   config/data/state/cache/runtime tree removed afterwards, and no desktop
#   session (DISPLAY, WAYLAND_*, the session bus and keyring sockets removed).
set -euo pipefail

if [[ $# -eq 0 ]]; then
    echo "usage: $0 COMMAND [ARG ...]" >&2
    exit 2
fi
repo=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

steps=(runtime audio)
[[ ${TILO_TEST_NTFS_FIXTURES:-1} == 0 ]] || steps+=(ntfs)
env_file=$(mktemp)
# This process becomes the test environment's (exec below) and waits for
# COMMAND, so the audio endpoint ends with COMMAND.
if ! NEXTEST_ENV=$env_file TILO_TEST_ENV_OWNER=$$ bash "$repo/tools/test-env-setup.sh" "${steps[@]}"; then
    rm -f -- "$env_file"
    exit 1
fi
while IFS= read -r line; do
    export "${line%%=*}=${line#*=}"
done <"$env_file"
rm -f -- "$env_file"

# The fixture cache lives under the caller's HOME; the command gets a fresh one.
exec bash "$repo/tools/with-test-environment.sh" "$@"
