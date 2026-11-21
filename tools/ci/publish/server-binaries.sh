#!/usr/bin/env bash
# The programs the tilo.sh server images run, compiled by t's own check and cached like any other
# build here: the API server, the VM host's service, its jail helper and its member, the
# server's daemon, its admin and its enroll program with the filesystem replica, and the server OS's
# boot and update tools. The images package what this leaves (api-build.sh, vm-host-build.sh,
# server-build.sh, host-build.sh); none compiles Rust inside its build, so an image after a change
# costs that crate's compile and a layering run. `host-dev` is what the development server image
# adds to the server OS (host-dev-build.sh): the dev-VM daemon and its CLI, compiled by the same
# check image and toolchain as `host`, so a layer built later the same way passes the daemon's
# layer check there.
#
# usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, server, host or host-dev)
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-binaries.sh IMAGE OUT    (IMAGE: api, vm-host, server, host or host-dev)'
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
    packages=(-p linux-vm --bin tilo-linux-vm --bin tilo-hosted-jail -p tilo-runtime --bin tilo-runtime)
    bins=(tilo-linux-vm tilo-hosted-jail tilo-runtime) ;;
  server)
    # The server and the filesystem replica in one feature resolution, as the image compiled them:
    # separate Cargo invocations recompile the shared dependencies whose selected features differ
    # by package root.
    packages=(-p tilo-server -p tilo-fs --bins)
    bins=(tilo-server tilo-server-admin tilo-enroll fs) ;;
  host)
    # The server OS: the server and replica, the hosted-VM service and its jail helper, the API
    # server with the program it starts a task's tilolang programs in (it finds it beside
    # itself), the boot tools the update log and the boot-entry pass read, and the early-boot
    # unlock chain its encrypted root needs (tilo-auth, tilo-rollback, the sign-in screen the
    # root discovery steps aside from, and the sibling factor's enrolment program), and tilo-crypto,
    # which verifies the signed boot chain a release adds to the image. One feature
    # resolution, so the shared workspace crates compile once.
    packages=(-p tilo-server -p tilo-fs --bin tilo-server --bin tilo-server-admin \
                --bin tilo-enroll --bin fs
              -p linux-vm --bin tilo-linux-vm --bin tilo-hosted-jail
              -p tilo-api --bin tilo-api
              -p tilolang-process --bin tilolang-process
              -p tilo-esp-write --bin tilo-esp-write
              -p tilo-image-policy-check --bin tilo-image-policy-check
              -p tilo-device-identity --bin tilo-image-trust
              -p tilo-board-successor --bin tilo-board-successor
              -p tilo-rollback --bin tilo-rollback
              -p tilo-auth --bin tilo-auth
              -p tilo-boot-signin --bin tilo-boot-signin
              -p tilo-crypto --bin tilo-crypto
              -p tilo-boot-order --bin tilo-default-start)
    bins=(tilo-server tilo-server-admin tilo-enroll fs tilo-linux-vm tilo-hosted-jail tilo-api
          tilolang-process
          tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor
          tilo-rollback tilo-auth tilo-boot-signin tilo-crypto tilo-default-start) ;;
  host-dev)
    # The dev-VM daemon and its CLI, as os/components.toml declares them, so this names no
    # binary of its own.
    read -ra packages <<<"$(python3 os/scripts/install-components.py --dry-run --format cargo vmd vm-cli)"
    mapfile -t bins < <(python3 os/scripts/install-components.py --dry-run --format bins vmd vm-cli) ;;
  *)
    die "unknown server image $image: api, vm-host, server, host or host-dev" ;;
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
