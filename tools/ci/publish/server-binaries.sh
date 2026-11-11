#!/usr/bin/env bash
# The programs the tilo.sh server images run, compiled by t's own check and cached like any other
# build here: the relay and the agent host's daemon and its helper programs, the front door's door,
# or the VM host's service, its jail helper and its member. The images package what this leaves
# (relay-build.sh, front-door-build.sh, vm-host-build.sh); none compiles Rust inside its build, so
# an image after a change costs that crate's compile and a layering run.
#
# usage: server-binaries.sh IMAGE OUT    (IMAGE: front-door, relay or vm-host)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-binaries.sh IMAGE OUT    (IMAGE: front-door, relay or vm-host)'
image=${1:?$usage}
out=${2:?$usage}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
# Thin LTO keeps these programs fast for a fraction of the memory and time the whole-program link
# took, and it is part of what ships: the image packages these bytes, so the flag is set here and
# nowhere else.
export CARGO_PROFILE_RELEASE_LTO=thin
case $image in
  front-door)
    packages=(-p tilo-front-door --bin tilo-front-door)
    bins=(tilo-front-door) ;;
  relay)
    # The daemon with its admin and enroll programs and the filesystem replica (the relay image),
    # and the workspace member the agent host runs beside it: one feature resolution for both
    # images, since tilo-runtime already depends on tilo-relay as a library.
    packages=(-p tilo-relay -p tilo-fs --bin tilo-relay --bin tilo-relay-admin --bin tilo-enroll --bin fs
              -p tilo-runtime --bin tilo-runtime)
    bins=(tilo-relay tilo-relay-admin tilo-enroll fs tilo-runtime) ;;
  vm-host)
    # tilo-runtime already depends on the runtime packages the host serves from; one feature
    # resolution for the three programs.
    packages=(-p legacy-vm --bin tilo-legacy --bin tilo-hosted-jail -p tilo-runtime --bin tilo-runtime)
    bins=(tilo-legacy tilo-hosted-jail tilo-runtime) ;;
  *)
    die "unknown server image $image: front-door, relay or vm-host" ;;
esac
rm -rf "$out"
mkdir -p "$out"
cargo build --locked --release "${packages[@]}"
for bin in "${bins[@]}"; do
  install -m 0755 "$target/release/$bin" "$out/$bin"
done
# What ships carries the link-time hardening .cargo/config.toml asks for, read back off the ELF.
bash os/scripts/verify-elf-hardening.sh "$out"
printf 'built %s\n' "${bins[*]}"
