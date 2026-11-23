#!/usr/bin/env bash
# The programs the tilo.sh server images run, compiled by t's own check and cached like any other
# build here: the API server, the VM host's service, its jail helper and its member, the
# server's daemon, its admin and its enroll program with the filesystem replica, and the server OS's
# boot and update tools. The images package what this leaves (api-build.sh, vm-host-build.sh,
# server-build.sh, host-build.sh); none compiles Rust inside its build, so an image after a change
# costs that crate's compile and a layering run.
#
# Several images may be named at once: they are one feature resolution and one build, which is what
# the development server image is (host-build.sh --dev names `host host-dev`, so every program it
# carries comes from one compilation). `host-dev` is what the development server image adds to the
# server OS: the runtime that runs the device's steps and the dev-VM CLI, compiled by the same
# check image and toolchain as `host`, so a layer built later the same way passes the device's
# layer check there.
#
# usage: server-binaries.sh IMAGE [IMAGE...] OUT    (IMAGE: api, vm-host, server, host or host-dev)
#        server-binaries.sh --list IMAGE [IMAGE...]  the programs it would build, one per line
set -euo pipefail
. "$(dirname -- "${BASH_SOURCE[0]}")/lib.sh"
usage='usage: server-binaries.sh IMAGE [IMAGE...] OUT | --list IMAGE [IMAGE...]    (IMAGE: api, vm-host, server, host or host-dev)'
list=false
if [[ ${1:-} == --list ]]; then
  list=true
  shift
  (($#)) || die "$usage"
  images=("$@")
else
  (($# >= 2)) || die "$usage"
  out=${!#}
  images=("${@:1:$#-1}")
fi
cd "$repo_root"
target=${CARGO_TARGET_DIR:-$repo_root/target}
# Thin LTO keeps these programs fast for a fraction of the memory and time the whole-program link
# took, and it is part of what ships: the image packages these bytes, so thin is the default and
# set here and nowhere else. A development image takes TILO_SERVER_LTO=off, the release profile
# without LTO, its codegen units spread and no line tables, as os/image/compile.sh builds the
# development OS: the compiler cache keeps a one-crate change to that crate's compile and the
# links instead of the whole-program relink.
: "${TILO_SERVER_LTO:=thin}"
case $TILO_SERVER_LTO in
  thin) export CARGO_PROFILE_RELEASE_LTO=thin ;;
  off)
    export CARGO_PROFILE_RELEASE_LTO=off CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16 \
           CARGO_PROFILE_RELEASE_DEBUG=0 ;;
  *) die "TILO_SERVER_LTO must be thin or off, not $TILO_SERVER_LTO" ;;
esac
packages=() bins=()
for image in "${images[@]}"; do
  case $image in
    api)
      # The API server starts a task's tilolang programs in tilolang-process, which it finds
      # beside itself: one feature resolution for the two.
      packages+=(-p tilo-api --bin tilo-api -p tilolang-process --bin tilolang-process)
      bins+=(tilo-api tilolang-process) ;;
    vm-host)
      # tilo-runtime already depends on the runtime packages the host serves from; one feature
      # resolution for its programs. The host joins its person's identity on first start with
      # `tilo login` and the ticket it was given, which reaches the daemon beside it, and whoever
      # starts it reads the device key that login left with `tilo device key`.
      packages+=(-p linux-vm --bin tilo-linux-vm --bin tilo-hosted-jail -p tilo-runtime --bin tilo-runtime \
                 -p tilo-cli --bin tilo)
      bins+=(tilo-linux-vm tilo-hosted-jail tilo-runtime tilo) ;;
    server)
      # The command and the daemon beside it are what a volume first booting as one of a person's
      # devices runs: `tilo login` spends the ticket it was given, and reaches its own daemon,
      # which the two programs find beside themselves.
      packages+=(-p tilo-server --bin tilo-server --bin tilo-server-admin \
                 -p tilo-cli --bin tilo -p tilo-runtime --bin tilo-runtime)
      bins+=(tilo-server tilo-server-admin tilo tilo-runtime) ;;
    host)
      # The server OS: the server and replica, the hosted-VM service and its jail helper, the API
      # server with the program it starts a task's tilolang programs in (it finds it beside
      # itself), the boot tools the update log and the boot-entry pass read, and the early-boot
      # unlock chain its encrypted root needs (tilo-auth, tilo-rollback, the sign-in screen the
      # root discovery steps aside from, and the sibling factor's enrolment program), and tilo-crypto,
      # which verifies the signed boot chain a release adds to the image. One feature
      # resolution, so the shared workspace crates compile once. The server itself is the
      # server image's components in os/components.toml (`images = ["host"]`), the entry a swap
      # into a server builds from too.
      read -ra host_components <<<"$(python3 os/scripts/install-components.py --dry-run --format cargo --all --image host)"
      mapfile -t host_bins < <(python3 os/scripts/install-components.py --dry-run --format bins --all --image host)
      packages+=("${host_components[@]}"
                 -p tilo-fs --bin fs
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
      bins+=("${host_bins[@]}" fs tilo-linux-vm tilo-hosted-jail tilo-api
             tilolang-process
             tilo-esp-write tilo-image-policy-check tilo-image-trust tilo-board-successor
             tilo-rollback tilo-auth tilo-boot-signin tilo-crypto tilo-default-start) ;;
    host-dev)
      # The runtime that runs the device's steps and the dev-VM CLI, as os/components.toml
      # declares them, so this names no binary of its own.
      read -ra dev_components <<<"$(python3 os/scripts/install-components.py --dry-run --format cargo native-runtime-host device-steps vm-cli)"
      packages+=("${dev_components[@]}")
      mapfile -t dev_bins < <(python3 os/scripts/install-components.py --dry-run --format bins native-runtime-host device-steps vm-cli)
      bins+=("${dev_bins[@]}") ;;
    *)
      die "unknown server image $image: api, vm-host, server, host or host-dev" ;;
  esac
done
if $list; then
  printf '%s\n' "${bins[@]}"
  exit 0
fi
rm -rf "$out"
mkdir -p "$out"
cargo build --locked --release "${packages[@]}"
for bin in "${bins[@]}"; do
  install -m 0755 "$target/release/$bin" "$out/$bin"
done
# What ships carries the link-time hardening .cargo/config.toml asks for, read back off the ELF.
bash os/scripts/verify-elf-hardening.sh "$out"
printf 'built %s\n' "${bins[*]}"
