#!/usr/bin/env bash
# The programs the tilo.sh server images run, compiled by t's own check and cached like any other
# build here: the API server, the VM host's service, its jail helper and its member, the
# server's daemon, its admin and its enroll program with the filesystem replica, and the server OS's
# boot and update tools. The images package what this leaves (api-build.sh, vm-host-build.sh,
# server-build.sh, host-build.sh); none compiles Rust inside its build, so an image after a change
# costs that crate's compile and a layering run.
#
# usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, server or host)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, server or host)'
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
    # The API server starts a task's tilolang programs in tilolang-process, which it finds
    # beside itself: one feature resolution for the two.
    packages=(-p tilo-api --bin tilo-api -p tilolang-process --bin tilolang-process)
    bins=(tilo-api tilolang-process) ;;
  vm-host)
    # tilo-runtime already depends on the runtime packages the host serves from; one feature
    # resolution for the three programs.
    packages=(-p legacy-vm --bin tilo-legacy --bin tilo-hosted-jail -p tilo-runtime --bin tilo-runtime)
    bins=(tilo-legacy tilo-hosted-jail tilo-runtime) ;;
  server)
    # The server and the filesystem replica in one feature resolution, as the image compiled them:
    # separate Cargo invocations recompile the shared dependencies whose selected features differ
    # by package root.
    packages=(-p tilo-server -p tilo-fs --bins)
    bins=(tilo-server tilo-server-admin tilo-enroll fs) ;;
  host)
    # The server OS: the server and replica, the hosted-VM service and its jail helper, the API
    # server, and the boot tools the update log and the boot-entry pass read. One feature
    # resolution, so the shared workspace crates compile once.
    packages=(-p tilo-server -p tilo-fs --bin tilo-server --bin tilo-server-admin \
                --bin tilo-enroll --bin fs
              -p legacy-vm --bin tilo-legacy --bin tilo-hosted-jail
              -p tilo-api --bin tilo-api
              -p tilo-esp-write --bin tilo-esp-write
              -p tilo-image-policy-check --bin tilo-image-policy-check
              -p tilo-user-identity --bin tilo-image-trust
              -p tilo-board-successor --bin tilo-board-successor)
    bins=(tilo-server tilo-server-admin tilo-enroll fs tilo-legacy tilo-hosted-jail tilo-api
          tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor) ;;
  *)
    die "unknown server image $image: api, vm-host, server or host" ;;
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
