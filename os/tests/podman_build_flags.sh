#!/bin/bash
# Every local podman build of an OS image builds independent stages in
# parallel and can read files it bind-mounts from an SELinux-labelled build
# context; the in-image cargo compile gets one job per CPU as far as memory
# allows, not a fixed cap.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "$repo/os/dev/_common.sh"

[ "${TILO_PODMAN_BUILD_FLAGS[*]}" = "--jobs 4 --security-opt label=disable" ] \
  || { echo "unexpected default flags: ${TILO_PODMAN_BUILD_FLAGS[*]}" >&2; exit 1; }
flags="$(TILO_PODMAN_BUILD_JOBS=9 bash -c "source '$repo/os/dev/_common.sh'; echo \"\${TILO_PODMAN_BUILD_FLAGS[*]}\"")"
[ "$flags" = "--jobs 9 --security-opt label=disable" ] || { echo "TILO_PODMAN_BUILD_JOBS ignored: $flags" >&2; exit 1; }

for script in os/dev/build-image.sh os/dev/build-dev-image.sh os/dev/local-inputs.sh; do
  grep -q 'build "${TILO_PODMAN_BUILD_FLAGS\[@\]}" -f' "$repo/$script" \
    || { echo "$script runs podman build without TILO_PODMAN_BUILD_FLAGS" >&2; exit 1; }
done

jobs="$(cargo_build_jobs 2)"
cpus="$(nproc)"
mem_jobs="$(awk '/^MemTotal:/ { print int($2 / 2097152) }' /proc/meminfo)"
want=$(( cpus < mem_jobs ? cpus : mem_jobs ))
[ "$jobs" = "$want" ] || { echo "cargo_build_jobs 2 = $jobs, want min($cpus CPUs, $mem_jobs by memory)" >&2; exit 1; }
[ "$(cargo_build_jobs 1000000)" = $(( cpus < 8 ? cpus : 8 )) ] || { echo "no memory for one job must fall back to 8" >&2; exit 1; }

printf 'ok: podman builds run stages in parallel with label=disable; cargo jobs follow CPUs and memory\n'
