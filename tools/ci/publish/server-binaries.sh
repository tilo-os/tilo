#!/usr/bin/env bash
# The programs the tilo.sh server images run, compiled by t's own check and cached like any other
# build here: the API server, the VM host's service, its jail helper and its member, the
# relay's daemon, its admin and its enroll program with the filesystem replica, or the agent host's
# runtime. The images package what this leaves (api-build.sh, vm-host-build.sh,
# relay-build.sh, agent-host-build.sh); none compiles Rust inside its build, so an image after a
# change costs that crate's compile and a layering run.
#
# usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, relay or agent-host)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, relay or agent-host)'
image=${1:?$usage}
out=${2:?$usage}
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
# Thin LTO keeps these programs fast for a fraction of the memory and time the whole-program link
# took, and it is part of what ships: the image packages these bytes, so the flag is set here and
# nowhere else.
export CARGO_PROFILE_RELEASE_LTO=thin
case $image in
  api)
    packages=(-p tilo-api --bin tilo-api)
    bins=(tilo-api) ;;
  vm-host)
    # tilo-runtime already depends on the runtime packages the host serves from; one feature
    # resolution for the three programs.
    packages=(-p legacy-vm --bin tilo-legacy --bin tilo-hosted-jail -p tilo-runtime --bin tilo-runtime)
    bins=(tilo-legacy tilo-hosted-jail tilo-runtime) ;;
  relay)
    # The relay and the filesystem replica in one feature resolution, as the image compiled them:
    # separate Cargo invocations recompile the shared dependencies whose selected features differ
    # by package root.
    packages=(-p tilo-relay -p tilo-fs --bins)
    bins=(tilo-relay tilo-relay-admin tilo-enroll fs) ;;
  agent-host)
    # The headless runtime and the enroll CLI, one graph: tilo-runtime depends on tilo-relay as a
    # library.
    packages=(-p tilo-runtime -p tilo-relay --bin tilo-runtime --bin tilo-enroll)
    bins=(tilo-runtime tilo-enroll) ;;
  *)
    die "unknown server image $image: api, vm-host, relay or agent-host" ;;
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
