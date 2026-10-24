#!/bin/bash
# Turn a bootc container image into a bootable qcow2 — the job
# bootc-image-builder used to do here.
#
#   ./os/dev/disk-image.sh IMAGE OUTDIR [options]
#
#     --composefs             composefs backend (implied when the image has a UKI)
#     --bootloader systemd|grub|none
#     --size 20G              whole-disk size (raised if the parts don't fit)
#     --root-min 12G          minimum root partition size
#     --root-max 40G          optional root partition ceiling
#     --rootfs btrfs|ext4     root filesystem (default: btrfs)
#     --esp 2G                ESP size
#     --boot-gap 16M          free space before the ESP (Rockchip loader gap)
#     --rockchip-loader DIR   idbloader.img + u-boot.itb written into that gap
#     --retain 3              rollback generations the ESP must hold (see below)
#     --uki-budget 160M       per-generation kernel budget used by that check
#     --karg K=V              extra kernel argument (repeatable; unsealed only)
#     --no-default-kargs      drop the implicit rw + console=tty0,ttyS0
#     --user NAME:PASS[:G,G]  create a login account in the installed system
#     --target-imgref REF     what the installed system updates FROM
#     --sealed-source-imgref REF
#                            digest-equivalent registry mirror used for a sealed
#                            install without first importing IMAGE locally
#     --firstboot-seed FILE   install an installer-format NAME:SHA512_CRYPT seed;
#                            create the account on first boot, never in this step
#     --bootc-helper FILE     execute this bootc (an ELF for this host's
#                            architecture) instead of the image's /usr/bin/bootc
#     --reseal-uki            with a helper built from Containerfile.bootc-helper:
#                            let install rewrite a stale composefs= seal to
#                            its own digest and print it (bootc#2334)
#     --offline-target-dir DIR
#                            copy a verified, digest-pinned OCI target into the
#                            live installer's writable /var for offline installs
#     --etc-overlay DIR       add files to THIS disk's installed /etc (release
#                            gate only; os/dev/validation-access.sh). Limited to
#                            systemd/system, verity.d, pam.d/sshd, and the
#                            validation-only boot diagnostic script
#     --raw                   leave the raw image, skip the qcow2 conversion
#     --direct-boot-bundle DIR
#                            export this dev disk's installed UKI sections
#                            and ESP PARTUUID for WHPX direct kernel boot
#
#   TILO_ROOT_PODMAN_STORE=DIR  root podman, which runs bootc, keeps its image
#                            store and scratch in DIR instead of
#                            /var/lib/containers and /var/tmp (a build host
#                            whose root filesystem is small)
#   TILO_ROOT_PODMAN_STORAGE_CONF=FILE  explicitly pass a root storage.conf
#                            through sudo (for a native overlay store)
#   TILO_QCOW2_COMPRESS=1      write zstd clusters in the first qcow2 conversion;
#                            useful for the dev-disk runner's limited scratch
#
# Why not bib: bib has no composefs, UKI or backend-selection surface anywhere
# in its config, and the request for one (osbuild/bootc-image-builder#1164) has
# been idle since December. What bib does at this step is wrap the two commands
# below, so calling them directly is less machinery rather than more:
#
#   systemd-repart               partition + mkfs from a declarative definition
#   bootc install to-filesystem  install into the mounted result
#
# What bib also provided, and how it is replaced:
#
#   --type qcow2       qemu-img convert at the end
#   --rootfs btrfs     Format=btrfs in the repart definition
#   --chown            chown at the end
#   customizations.user        --user (chroot useradd into the installed root)
#   customizations.kernel      --karg, but see the note on sealed images below
#   customizations.filesystem  --root-min / --esp
#
# Sealed images (composefs + UKI): the kernel command line is signed into the
# UKI at *build* time, so --karg cannot reach it. A karg that has to be on the
# command line of a sealed image belongs in that image's
# /usr/lib/bootc/kargs.d/ — which is where `bootc container ukify` reads it
# from — or in a separately signed UKI addon. Passing --karg for an image that
# carries a UKI is refused here rather than silently dropped. --karg still works
# for unsealed installs, and is passed through unchanged.
#
# Two requirements of a sealed image that this step CANNOT satisfy, because they
# belong to the image build (os/Containerfile) rather than to the disk:
#
#   * The stage that runs `bootc container ukify` needs a host-backed /var/tmp.
#     Computing the composefs digest builds a temporary composefs repository
#     there and writing an object needs O_TMPFILE, which overlayfs — the
#     filesystem a build stage's root is — does not support. The error is
#     "Creating object tmpfile: ... Operation not supported (os error 95)" and it
#     names neither /var/tmp nor overlayfs. `--mount=type=cache,target=/var/tmp`
#     fixes it; see os/dev/composefs/Containerfile.
#   * The build has to run TWICE until bootc#2334 is fixed: `bootc container
#     ukify` and `bootc install` compute different composefs digests for the same
#     image, so the first install is refused, and the UKI has to be rebuilt
#     sealing the digest install asked for. The expected value is stable across
#     that rebuild, so two passes converge. os/dev/composefs/seal.sh is the loop.
#     When install fails that way, the message below says so and prints the
#     digest to seal.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# Root podman runs bootc out of the image, so it holds a copy of it: by default
# in /var/lib/containers, staged through /var/tmp.
ROOT_PODMAN_ARGS=(); ROOT_PODMAN_ENV=(); ROOT_CONTAINERS=/var/lib/containers
if [ -n "${TILO_ROOT_PODMAN_STORE:-}" ]; then
  mkdir -p "$TILO_ROOT_PODMAN_STORE"
  TILO_ROOT_PODMAN_STORE="$(cd "$TILO_ROOT_PODMAN_STORE" && pwd)"
  ROOT_CONTAINERS="$TILO_ROOT_PODMAN_STORE/containers"
  sudo -n mkdir -p "$ROOT_CONTAINERS/storage" "$TILO_ROOT_PODMAN_STORE/run" "$TILO_ROOT_PODMAN_STORE/tmp"
  ROOT_PODMAN_ARGS=(--root "$ROOT_CONTAINERS/storage" --runroot "$TILO_ROOT_PODMAN_STORE/run")
  ROOT_PODMAN_ENV=("TMPDIR=$TILO_ROOT_PODMAN_STORE/tmp")
fi
if [ -n "${TILO_ROOT_PODMAN_STORAGE_CONF:-}" ]; then
  ROOT_PODMAN_ENV+=("CONTAINERS_STORAGE_CONF=$TILO_ROOT_PODMAN_STORAGE_CONF")
fi
root_podman() {
  sudo -n env "${ROOT_PODMAN_ENV[@]+"${ROOT_PODMAN_ENV[@]}"}" \
    podman "${ROOT_PODMAN_ARGS[@]+"${ROOT_PODMAN_ARGS[@]}"}" "$@"
}

IMAGE=""; OUTDIR=""
COMPOSEFS=auto; BOOTLOADER=""; SIZE=""; ROOT_MIN=12G; ROOT_MAX=""; ROOTFS=btrfs; ESP_SIZE=2G
BOOT_GAP=""; ROCKCHIP_LOADER=""
RETAIN="${TILO_RETAINED_GENERATIONS:-3}"; UKI_BUDGET=160M
KARGS=(); USERSPEC=""; TARGET_IMGREF=""; SEALED_SOURCE_IMGREF=""
FIRSTBOOT_SEED=""
BOOTC_HELPER=""
RESEAL_UKI=0
DIRECT_SEALED_SOURCE=0
OFFLINE_TARGET_DIR=""
ETC_OVERLAY=""
DIRECT_BOOT_DIR=""
KEEP_RAW=0; DEFAULT_KARGS=yes

while [ $# -gt 0 ]; do
  case "$1" in
    --composefs) COMPOSEFS=yes ;;
    --no-composefs) COMPOSEFS=no ;;
    --bootloader) BOOTLOADER="$2"; shift ;;
    --size) SIZE="$2"; shift ;;
    --root-min) ROOT_MIN="$2"; shift ;;
    --root-max) ROOT_MAX="$2"; shift ;;
    --rootfs) ROOTFS="$2"; shift ;;
    --esp) ESP_SIZE="$2"; shift ;;
    --boot-gap) BOOT_GAP="$2"; shift ;;
    --rockchip-loader) ROCKCHIP_LOADER="$2"; shift ;;
    --retain) RETAIN="$2"; shift ;;
    --uki-budget) UKI_BUDGET="$2"; shift ;;
    --karg) KARGS+=("--karg" "$2"); shift ;;
    --no-default-kargs) DEFAULT_KARGS=no ;;
    --user) USERSPEC="$2"; shift ;;
    --target-imgref) TARGET_IMGREF="$2"; shift ;;
    --sealed-source-imgref) SEALED_SOURCE_IMGREF="$2"; shift ;;
    --firstboot-seed) FIRSTBOOT_SEED="$2"; shift ;;
    --bootc-helper) BOOTC_HELPER="$2"; shift ;;
    --reseal-uki) RESEAL_UKI=1 ;;
    --offline-target-dir) OFFLINE_TARGET_DIR="$2"; shift ;;
    --etc-overlay) ETC_OVERLAY="$2"; shift ;;
    --direct-boot-bundle) DIRECT_BOOT_DIR="$2"; shift ;;
    --raw) KEEP_RAW=1 ;;
    -h|--help) sed -n '2,22p' "$0"; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) if [ -z "$IMAGE" ]; then IMAGE="$1"; elif [ -z "$OUTDIR" ]; then OUTDIR="$1";
       else die "unexpected argument: $1"; fi ;;
  esac
  shift
done
[ -n "$IMAGE" ] && [ -n "$OUTDIR" ] || { sed -n '2,22p' "$0"; exit 1; }
case "$ROOTFS" in
  btrfs|ext4) ;;
  *) die "--rootfs must be btrfs or ext4, got: $ROOTFS" ;;
esac
if [ -n "$BOOT_GAP" ]; then
  case "${BOOT_GAP^^}" in
    *M|*MIB) boot_gap_mib="${BOOT_GAP%%[Mm]*}" ;;
    *G|*GIB) boot_gap_mib=$(( ${BOOT_GAP%%[Gg]*} * 1024 )) ;;
    *) die "--boot-gap must be a whole number of MiB or GiB, got: $BOOT_GAP" ;;
  esac
  [[ "$boot_gap_mib" =~ ^[0-9]+$ ]] || die "--boot-gap must be a whole number of MiB or GiB, got: $BOOT_GAP"
  [ "$boot_gap_mib" -ge 16 ] || die "--boot-gap must be at least 16M so a Rockchip loader fits before the ESP"
fi
if [ -n "$ROCKCHIP_LOADER" ]; then
  [ -n "$BOOT_GAP" ] || die "--rockchip-loader requires --boot-gap"
  [ -d "$ROCKCHIP_LOADER" ] && [ ! -L "$ROCKCHIP_LOADER" ] \
    || die "--rockchip-loader must name a regular directory"
  for loader_name in idbloader.img u-boot.itb; do
    loader_path="$ROCKCHIP_LOADER/$loader_name"
    [ -f "$loader_path" ] && [ ! -L "$loader_path" ] \
      || die "--rockchip-loader is missing $loader_name"
  done
fi

if [ -n "$FIRSTBOOT_SEED" ]; then
  [ -f "$FIRSTBOOT_SEED" ] && [ ! -L "$FIRSTBOOT_SEED" ] \
    || die "--firstboot-seed must name a regular, non-symlink file"
  [ "$(wc -l < "$FIRSTBOOT_SEED")" -eq 1 ] \
    || die "--firstboot-seed must contain exactly one line"
  IFS= read -r firstboot_line < "$FIRSTBOOT_SEED" || true
  firstboot_user="${firstboot_line%%:*}"
  firstboot_hash="${firstboot_line#*:}"
  [[ "$firstboot_user" =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] \
    || die "--firstboot-seed contains an invalid account name"
  # Use the production first-boot parser as the authority rather than carrying
  # a second password-hash grammar in this disk tool.
  source "$(dirname "${BASH_SOURCE[0]}")/../usr/libexec/tilo-firstboot-user"
  valid_sha512_crypt "$firstboot_hash" \
    || die "--firstboot-seed must contain a complete SHA-512 crypt hash"
fi

# The overlay only ever adds validation access to a gate's own disk copy: a
# unit, the trust anchor for its extension, sshd's PAM stack, and its boot
# diagnostic script. Anything else is refused rather than silently written
# into an installed /etc.
if [ -n "$ETC_OVERLAY" ]; then
  [ -d "$ETC_OVERLAY" ] && [ ! -L "$ETC_OVERLAY" ] \
    || die "--etc-overlay must name a regular directory"
  while IFS= read -r entry; do
    case "$entry" in
      systemd/system/*.service|systemd/system/*.target.wants/*.service|verity.d/*.crt|pam.d/sshd) ;;
      tilo-validation-diagnostics.sh)
        [ -f "$ETC_OVERLAY/$entry" ] && [ ! -L "$ETC_OVERLAY/$entry" ] \
          || die "--etc-overlay diagnostics script must be a regular file"
        ;;
      *) die "--etc-overlay may not write /etc/$entry" ;;
    esac
  done < <(cd "$ETC_OVERLAY" && find . \( -type f -o -type l \) -printf '%P\n')
  if (cd "$ETC_OVERLAY" && find . ! -type d ! -type f ! -type l -print -quit) | grep -q .; then
    die "--etc-overlay contains a special file"
  fi
fi

if [ -n "$OFFLINE_TARGET_DIR" ]; then
  OFFLINE_TARGET_FLAGS=()
  case "${TILO_INSTALLER_DEV_TARGET:-0}" in
    0) ;;
    1) OFFLINE_TARGET_FLAGS=(--development) ;;
    *) die "TILO_INSTALLER_DEV_TARGET must be 0 or 1" ;;
  esac
  [ -d "$OFFLINE_TARGET_DIR" ] && [ ! -L "$OFFLINE_TARGET_DIR" ] \
    || die "--offline-target-dir must name a regular directory"
  python3 "$(dirname "${BASH_SOURCE[0]}")/installer-offline-target.py" \
    verify "$OFFLINE_TARGET_DIR" "${OFFLINE_TARGET_FLAGS[@]}" \
    || die "offline target OCI layout failed validation"
fi

if [ -n "$BOOTC_HELPER" ]; then
  [ -f "$BOOTC_HELPER" ] && [ ! -L "$BOOTC_HELPER" ] && [ -x "$BOOTC_HELPER" ] \
    || die "--bootc-helper must name an executable regular, non-symlink file"
  python3 - "$BOOTC_HELPER" <<'PY' \
    || die "--bootc-helper must be an ELF executable for $(uname -m)"
import platform
import struct
import sys

MACHINES = {"aarch64": 183, "x86_64": 62}

with open(sys.argv[1], "rb") as source:
    header = source.read(20)
if len(header) != 20 or header[:4] != b"\x7fELF" or header[5] not in (1, 2):
    raise SystemExit(1)
byteorder = "<" if header[5] == 1 else ">"
if struct.unpack_from(byteorder + "H", header, 18)[0] != MACHINES.get(platform.machine()):
    raise SystemExit(1)
PY
  BOOTC_HELPER="$(readlink -f "$BOOTC_HELPER")"
fi
[ "$RESEAL_UKI" = 0 ] || [ -n "$BOOTC_HELPER" ] || die "--reseal-uki needs --bootc-helper"

if [ -z "$SEALED_SOURCE_IMGREF" ]; then
  podman image exists "$IMAGE" || die "$IMAGE is not in rootless podman storage"
else
  [ "$COMPOSEFS" = yes ] || die "--sealed-source-imgref requires --composefs"
  [[ "$IMAGE" =~ @sha256:[0-9a-f]{64}$ ]] \
    || die "--sealed-source-imgref requires digest-pinned IMAGE as the authoritative target-imgref"
  command -v skopeo >/dev/null || die "--sealed-source-imgref requires skopeo to verify the mirror digest"
  source_tls=(--tls-verify=false)
  if [ "${SEALED_SOURCE_IMGREF#docker://}" = "$IMAGE" ]; then
    # An authoritative signed source is not an unsigned construction mirror.
    DIRECT_SEALED_SOURCE=1
    source_tls=()
  fi
  source_identity="$(skopeo inspect "${source_tls[@]}" --format '{{.Name}} {{.Digest}}' \
    "docker://${SEALED_SOURCE_IMGREF#docker://}")" \
    || die "cannot inspect sealed registry source $SEALED_SOURCE_IMGREF"
  read -r source_repository source_digest <<< "$source_identity"
  [ -n "$source_repository" ] && [ -n "$source_digest" ] \
    || die "sealed registry source inspection returned no repository or digest"
  target_digest="sha256:${IMAGE##*@sha256:}"
  [ "$source_digest" = "$target_digest" ] || die "sealed registry source digest is $source_digest,
  but authoritative IMAGE is $target_digest. Refusing to install different content."
  # From this point on, address the mirror by the digest that was just compared
  # with the authoritative image. A mutable convenience tag is useful for
  # populating the mirror, but it must not remain in the install invocation.
  SEALED_SOURCE_IMGREF="$source_repository@$source_digest"
fi

# --- sizes -------------------------------------------------------------------
# Everything is compared in MiB so a check can talk about "864 MiB" instead of
# about strings that happen to differ.
to_mib() {
  local v="${1^^}"
  case "$v" in
    *T|*TIB) echo $(( ${v%%T*} * 1024 * 1024 )) ;;
    *G|*GIB) echo $(( ${v%%G*} * 1024 )) ;;
    *M|*MIB) echo "${v%%M*}" ;;
    *K|*KIB) echo $(( ${v%%K*} / 1024 )) ;;
    *[0-9]) echo $(( v / 1024 / 1024 )) ;;
    *) die "cannot parse size: $1" ;;
  esac
}
# --- backend ------------------------------------------------------------------
# A UKI in the image means bootc selects the composefs backend by itself, and
# the install must agree — so detect rather than make the caller remember.
HAS_UKI=no; WANTS_SYSTEMD_BOOT=no
if [ -n "$SEALED_SOURCE_IMGREF" ]; then
  # The explicit option names the source as sealed and its digest was verified
  # above. Avoid importing a multi-gigabyte image only to rediscover that fact.
  HAS_UKI=yes
elif podman run --rm --entrypoint /bin/sh "$IMAGE" \
     -c 'ls /boot/EFI/Linux/*.efi >/dev/null 2>&1'; then
  HAS_UKI=yes
fi
# The image's own install config decides the bootloader (Tilo ships
# `bootloader = "systemd"` in /usr/lib/bootc/install/40-tilo.toml), and that
# choice constrains the backend: `bootc install` refuses systemd-boot on the
# ostree backend with "bootupd is required for ostree-based installs" — bootupd
# only plants GRUB. So an image asking for systemd-boot needs the composefs
# backend, whether or not it has a UKI. This is also why the last bib-built
# desktop qcow2 could not have had systemd-boot: bib hits the same refusal, from
# the same drop-in.
if [ -z "$SEALED_SOURCE_IMGREF" ] && podman run --rm --entrypoint /bin/sh "$IMAGE" \
     -c 'grep -rqs "bootloader *= *\"systemd\"" /usr/lib/bootc/install/'; then
  WANTS_SYSTEMD_BOOT=yes
fi
if [ "$COMPOSEFS" = auto ]; then
  if [ "$HAS_UKI" = yes ]; then
    COMPOSEFS=yes
    msg "the image carries a UKI — using the composefs backend"
  elif [ "$WANTS_SYSTEMD_BOOT" = yes ]; then
    COMPOSEFS=yes
    msg "the image asks for systemd-boot — using the composefs backend (unsealed;"
    msg "  the ostree backend can only install GRUB, via bootupd)"
  else
    COMPOSEFS=no
  fi
elif [ "$COMPOSEFS" = no ] && [ "$HAS_UKI" = yes ]; then
  die "$IMAGE carries a UKI, which makes bootc select the composefs backend on its own.
  --no-composefs would produce an install that disagrees with the image. Drop it,
  or build the image without a UKI."
elif [ "$COMPOSEFS" = no ] && [ "$WANTS_SYSTEMD_BOOT" = yes ]; then
  die "$IMAGE asks for systemd-boot in /usr/lib/bootc/install/, which the ostree
  backend cannot install (\"bootupd is required for ostree-based installs\").
  Drop --no-composefs, or change the image's bootloader to \"grub\"."
fi

# bib's implicit kargs, which nothing in the image supplies: `rw` because bootc
# mounts the root read-only otherwise, and the framebuffer and architecture's
# serial consoles because that is how
# a headless boot leaves a log the VM toolkit can read (os/dev/vm.sh serial).
# A sealed image cannot take kargs here at all, so for that layout they have to
# be in the image's own kargs.d — the message below says so.
if [ ${#KARGS[@]} -eq 0 ] && [ "$HAS_UKI" = no ] && [ "$DEFAULT_KARGS" = yes ]; then
  KARGS=(--karg rw --karg console=tty0 --karg console=ttyAMA0 --karg console=ttyS0)
fi

if [ ${#KARGS[@]} -gt 0 ] && [ "$HAS_UKI" = yes ]; then
  die "--karg cannot reach a sealed image's kernel command line.
  The command line is signed into the UKI at build time, so an argument added
  here would be silently absent at boot. Put it in the image's
  /usr/lib/bootc/kargs.d/ — \`bootc container ukify\` reads that directory — or
  ship it as a separately signed UKI addon."
fi

# Where the ESP goes, which is not the same on both layouts:
#
#   composefs  /boot IS the ESP. systemd-boot reads the loader entries from there,
#              and on that layout the kernels live beside them — a UKI per
#              generation when sealed, a vmlinuz + initramfs pair when not.
#   ostree     /boot carries the kernels, the initramfs, the BLS entries and
#              ostree's own bootversion state, all of which need xattrs and none of
#              which vfat can hold. So /boot stays a directory on the btrfs root and
#              the ESP mounts under it at /boot/efi (bib used a separate ext4 /boot
#              partition for the same reason). Mounting the ESP at /boot instead
#              fails during "Creating ostree deployment" with
#              "fsetxattr(security.selinux): Operation not supported", which names
#              neither vfat nor /boot.
if [ "$COMPOSEFS" = yes ]; then BOOT_IS_ESP=yes; else BOOT_IS_ESP=no; fi

# --- sizes --------------------------------------------------------------------
# The filesystem that holds the kernels has to hold one per generation that must
# remain bootable — a ~134 MB UKI on the sealed layout, a vmlinuz plus a ~120 MB
# initramfs on the ostree one. So its size is arithmetic on the rollback retention
# window, and the two settings have to agree:
#
#   capacity >= (retained_generations + RETAINED_NEWER_GENERATIONS) * per-generation
#               + slack for the bootloader and filesystem overhead
#
# retained_generations is tilo-rollback's window (`Config::default()` in
# crates/system/tilo-rollback/src/lib.rs, default 3 — "this generation plus the two
# before it"). RETAINED_NEWER_GENERATIONS (2, retention.rs) covers a staged update
# whose kernel is already written before it has ever booted, plus a second staged
# behind it. At the defaults that is 5 * 160 MiB + 64 MiB = 864 MiB.
#
# Which filesystem it lands on follows the layout above: the ESP on the sealed
# layout, the root filesystem on the ostree one. Nothing here connected the two
# settings before, so raising the retention window could silently overflow a 2 GiB
# ESP at update time — hence a check rather than a comment.
# os/tests/esp_capacity.sh asserts the same arithmetic against the Rust default,
# so the two cannot drift apart unnoticed.
RETAINED_NEWER=2
# The repair-mode image. Exactly one at a time, not one per generation: it is
# derived on the machine from whichever UKI the running deployment boots
# (docs/security/repair-mode.md), so it is refreshed rather than accumulated.
# Counting it here is what keeps a machine from filling its ESP the first time a
# repair entry is made — which would happen at update time, in the field.
REPAIR_UKI=1
KERNEL_FS_SLACK_MIB=64
esp_mib="$(to_mib "$ESP_SIZE")"
root_mib="$(to_mib "$ROOT_MIN")"
if [ -n "$ROOT_MAX" ]; then
  root_max_mib="$(to_mib "$ROOT_MAX")"
  [ "$root_max_mib" -ge "$root_mib" ] \
    || die "--root-max $ROOT_MAX is smaller than --root-min $ROOT_MIN"
fi
uki_mib="$(to_mib "$UKI_BUDGET")"
[ "$RETAIN" -ge 1 ] 2>/dev/null || die "--retain must be a positive integer, got: $RETAIN"
kernels_need=$(( (RETAIN + RETAINED_NEWER + REPAIR_UKI) * uki_mib + KERNEL_FS_SLACK_MIB ))
if [ "$BOOT_IS_ESP" = yes ]; then
  kernels_have="$esp_mib"; kernels_flag="--esp $ESP_SIZE"; kernels_where="the ESP"
else
  kernels_have="$root_mib"; kernels_flag="--root-min $ROOT_MIN"; kernels_where="/boot on the root filesystem"
fi
if [ "$kernels_have" -lt "$kernels_need" ]; then
  die "$kernels_where is too small for the rollback retention window.
    $kernels_flag ($kernels_have MiB)
    needs $kernels_need MiB = ($RETAIN retained + $RETAINED_NEWER staged + $REPAIR_UKI repair) x $UKI_BUDGET + $KERNEL_FS_SLACK_MIB MiB slack
  Raise it, or lower --retain to at most $(( (kernels_have - KERNEL_FS_SLACK_MIB) / uki_mib - RETAINED_NEWER - REPAIR_UKI )).
  --retain must match tilo-rollback's retained_generations
  (crates/system/tilo-rollback/src/lib.rs); os/tests/esp_capacity.sh checks that."
fi

# The disk has to be big enough for the partitions asked for; growing it is free
# (the qcow2 stays sparse) and a caller passing --root-min without recomputing
# --size is the common case, so raise rather than refuse.
size_min=$(( esp_mib + root_mib + 128 ))
if [ -z "$SIZE" ]; then
  SIZE_MIB=$(( size_min > 20480 ? size_min : 20480 ))
else
  SIZE_MIB="$(to_mib "$SIZE")"
  if [ "$SIZE_MIB" -lt "$size_min" ]; then
    msg "raising the disk from ${SIZE_MIB}M to ${size_min}M: ESP $ESP_SIZE + root $ROOT_MIN does not fit"
    SIZE_MIB="$size_min"
  fi
fi
SIZE="${SIZE_MIB}M"

mkdir -p "$OUTDIR"
# Absolute, because Podman binds this directory into the installer container.
# A relative source would resolve against Podman's current working directory.
OUTDIR="$(cd "$OUTDIR" && pwd)"
# DrvFs is 9p inside WSL. Its raw loop-backed image can allocate the entire
# logical disk on Windows before qemu-img has a chance to make a sparse qcow2.
# Build on WSL's native ext4 filesystem, then export only the final qcow2.
if [ "$(stat -f -c %T "$OUTDIR")" = v9fs ]; then
  die "disk image output $OUTDIR is on a 9p filesystem; use a native Linux filesystem for the raw build, then export the qcow2"
fi
RAW="$OUTDIR/disk.raw"
QCOW="$OUTDIR/disk.qcow2"
MNT="$OUTDIR/.mnt"
DEFS="$OUTDIR/.repart.d"
LOG="$OUTDIR/.install.log"
ESP_VERIFY="$OUTDIR/.esp-verify"
ESP_STAGED=0
ESP_STAGE_BIND_MOUNTED=0
ESP_STAGE=""
ESP_UUID=""

cleanup() {
  if [ "$ESP_STAGE_BIND_MOUNTED" = 1 ]; then
    sudo -n umount "$ESP_AT" 2>/dev/null || true
    ESP_STAGE_BIND_MOUNTED=0
  fi
  sudo -n umount -R "$MNT" 2>/dev/null || true
  if [ -n "${LOOP_NODE_HOLDER_PID:-}" ]; then
    printf 'release\n' >&"$LOOP_NODE_HOLDER_IN" || true
    wait "$LOOP_NODE_HOLDER_PID" 2>/dev/null || true
  fi
  [ -n "${LOOP:-}" ] && sudo -n losetup -d "$LOOP" 2>/dev/null || true
  if [ -n "$ESP_STAGE" ] && [ -d "$ESP_STAGE" ]; then
    sudo -n rm -rf -- "$ESP_STAGE" 2>/dev/null || true
    ESP_STAGE=""
  fi
  [ -z "${SOURCE_POLICY:-}" ] || rm -f "$SOURCE_POLICY"
  rm -rf "$DEFS" "$ESP_VERIFY"
}
trap cleanup EXIT

# --- an install source the composefs backend can read -------------------------
# The install source has to be a registry when the composefs backend is in play.
# Installing from containers-storage — including an image moved into root storage
# with `podman save | sudo podman load`, which is what os/build.sh used to do —
# fails inside the composefs pull with "Opening stream '': Creating new
# splitstream reader: Invalid splitstream content type". The same image pulled
# over HTTP installs. So a local registry is started and the image published to
# it; this is a new requirement of dropping bib.
REG=""; SRC_REF=""; RUNREF="$IMAGE"; TLS_ARGS=(); SOURCE_POLICY=""
# Ready only when this answers. A container named tilo-disk-registry can be
# exited while its port mapping still exists, and that name used to count as
# success even though 127.0.0.1:5000 was refusing connections.
registry_v2_up() {
  curl --connect-timeout 2 --max-time 4 -sf "http://127.0.0.1:$1/v2/" >/dev/null 2>&1
}
start_registry() {
  local want="${TILO_DISK_REG_PORT:-5000}" port running started=false
  if podman container exists tilo-disk-registry 2>/dev/null; then
    running="$(podman inspect -f '{{.State.Running}}' tilo-disk-registry 2>/dev/null)" || running=false
    if [ "$running" != true ]; then
      podman start tilo-disk-registry >/dev/null 2>&1 || true
      running="$(podman inspect -f '{{.State.Running}}' tilo-disk-registry 2>/dev/null)" || running=false
      started=true
    fi
    port="$(podman port tilo-disk-registry 5000/tcp 2>/dev/null | head -1)" || true
    port="${port##*:}"
    [ -n "$port" ] || port="$want"
    # Only a container we just started needs a moment to accept connections.
    # One that was already up, or that stayed exited, is checked once.
    if [ "$started" = true ] && [ "$running" = true ]; then
      for _ in $(seq 1 30); do
        registry_v2_up "$port" && break
        sleep 1
      done
    fi
    if registry_v2_up "$port"; then
      echo "localhost:$port"
      return
    fi
    msg "replacing tilo-disk-registry; http://127.0.0.1:$port/v2/ is not answering" >&2
    podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
  fi
  # Someone else's registry already listening (the composefs spike leaves one on
  # :5000) is fine to push to — one local registry is as good as another.
  if registry_v2_up "$want"; then
    msg "reusing the registry already listening on :$want" >&2
    echo "localhost:$want"
    return
  fi
  for port in $(seq "$want" $(( want + 9 ))); do
    podman run -d --name tilo-disk-registry -p "$port:5000" \
      docker.io/library/registry:2 >/dev/null 2>&1 || {
        podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
        continue
      }
    for _ in $(seq 1 30); do
      registry_v2_up "$port" && break
      sleep 1
    done
    if registry_v2_up "$port"; then
      msg "started a registry on :$port for the install source" >&2
      echo "localhost:$port"
      return
    fi
    podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
  done
  return 1
}

# A policy for the install's own run of the image that admits REPOSITORY alone. The image's
# runtime policy trusts its signed authoritative repository, not the transport the install
# reads it from, and bootc evaluates the running reference against it before installing; the
# file is bind-mounted over the running container's policy only, so the installed system keeps
# the policy shipped in the image.
source_policy() {
  SOURCE_POLICY="$OUTDIR/.source-policy.json"
  python3 - "$SOURCE_POLICY" "$1" <<'PY'
import json
import sys

path, repository = sys.argv[1:]
policy = {
    "default": [{"type": "reject"}],
    "transports": {
        "docker": {
            repository: [{"type": "insecureAcceptAnything"}],
        },
    },
}
with open(path, "w", encoding="utf-8") as output:
    json.dump(policy, output, separators=(",", ":"))
    output.write("\n")
PY
}

if [ -n "$SEALED_SOURCE_IMGREF" ]; then
  RUNREF="${SEALED_SOURCE_IMGREF#docker://}"
  SRC_REF="docker://$RUNREF"
  REG="${RUNREF%%/*}"
  TLS_ARGS=(--tls-verify=false)
  REGCONF="$OUTDIR/.registries.conf"
  if [ "$DIRECT_SEALED_SOURCE" = 1 ]; then
    TLS_ARGS=()
    printf '[[registry]]\nlocation = "%s"\ninsecure = false\n' "$REG" > "$REGCONF"
  else
  printf '[[registry]]\nlocation = "%s"\ninsecure = true\n' "$REG" > "$REGCONF"
  # The digest-verified, unsigned mirror is the one repository the install's run admits.
  source_policy "$source_repository"
  fi
  msg "using verified sealed registry source $SRC_REF (no local import or republish)"
elif [ "$COMPOSEFS" = yes ]; then
  REG="$(start_registry)" || die "could not bring up a local registry, and the composefs
  backend cannot install from container storage: pulling an image out of
  containers-storage (or one moved across with \`podman save | sudo podman load\`)
  fails with \"Invalid splitstream content type\". The disk step therefore needs a
  registry it can reach over HTTP. Start one and set TILO_DISK_REG_PORT, or install
  docker.io/library/registry:2 so this can start its own."
  # The source may be digest-pinned (`registry/owner/tilo@sha256:...`). Splitting
  # at the first colon turns that into the invalid repository `tilo@sha256`.
  # Isolate the final path component first, then remove either digest or tag.
  repo="$(basename "$IMAGE")"
  repo="${repo%%@*}"
  repo="${repo%%:*}"
  [ -n "$repo" ] || die "cannot derive a local registry repository from $IMAGE"
  msg "publishing $IMAGE to $REG/$repo:disk-install"
  podman push --tls-verify=false "$IMAGE" "$REG/$repo:disk-install" \
    || die "pushing $IMAGE to $REG failed. The composefs backend has to install from a
  registry (see the note above), so there is no local-storage fallback to take."
  SRC_REF="docker://$REG/$repo:disk-install"
  # bootc runs *out of* the image being installed, so root podman needs a copy
  # too. Running the one already in the registry avoids save|load — which is the
  # transfer that breaks the composefs pull — and makes one reference serve as
  # both the container to run and the install source.
  RUNREF="$REG/$repo:disk-install"
  # A dev image's policy admits only its published repository, not this scratch one.
  source_policy "$REG/$repo"
  TLS_ARGS=(--tls-verify=false)
  REGCONF="$OUTDIR/.registries.conf"
  printf '[[registry]]\nlocation = "%s"\ninsecure = true\n' "$REG" > "$REGCONF"
elif ! root_podman image exists "$IMAGE" 2>/dev/null; then
  msg "transfer $IMAGE into root container storage"
  podman save "$IMAGE" | root_podman load
fi

# The installed system updates from the image it was built from, not from the
# scratch registry this step published to. Without this the qcow2 would carry
# `localhost:5000/...:disk-install` as its update source and `bootc upgrade`
# would look for a registry that only existed during the build.
[ -n "$TARGET_IMGREF" ] || TARGET_IMGREF="$IMAGE"

# --- partition ----------------------------------------------------------------
mkdir -p "$DEFS"
printf '[Partition]\nType=esp\nFormat=vfat\nSizeMinBytes=%s\nSizeMaxBytes=%s\n' \
  "$ESP_SIZE" "$ESP_SIZE" > "$DEFS/01-esp.conf"
if [ -n "$BOOT_GAP" ]; then
  # systemd-repart places padding after a partition and starts the first one
  # at 1 MiB. A 15 MiB unformatted hole fills 1 MiB..16 MiB, which is where
  # U-Boot is stored; the ESP then starts at LBA 32768.
  printf '[Partition]\nType=linux-generic\nLabel=rockchip-loader\nSizeMinBytes=15M\nSizeMaxBytes=15M\n' \
    > "$DEFS/00-rockchip-gap.conf"
fi
printf '[Partition]\nType=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab\nLabel=Tilo root\nFormat=%s\nSizeMinBytes=%s\nGrowFileSystem=yes\n' \
  "$ROOTFS" "$ROOT_MIN" > "$DEFS/02-root.conf"
if [ -n "$ROOT_MAX" ]; then
  printf 'SizeMaxBytes=%s\n' "$ROOT_MAX" >> "$DEFS/02-root.conf"
fi

msg "1/4 systemd-repart -> $RAW (ESP $ESP_SIZE + $ROOTFS root >= $ROOT_MIN, disk $SIZE)"
rm -f "$RAW"
truncate -s "$SIZE" "$RAW"
# Pin 512-byte sectors. Without --sector-size, systemd-repart 255 and >= 257
# format a regular file's partitions with 4096-byte filesystem sectors, which
# makes the ESP a 4K-sector FAT that SD/eMMC/USB firmware cannot read. btrfs
# keeps its 4 KiB minimum block size; ext4 does not take a sector size.
sudo -n systemd-repart --empty=force --dry-run=no --discard=no --offline=false \
  --sector-size=512 --definitions="$DEFS" --size="$SIZE" "$RAW"
python3 "$SCRIPT_DIR/check-disk-sectors.py" "$RAW" >&2 \
  || die "systemd-repart did not lay out $RAW with 512-byte sectors"

LOOP="$(sudo -n losetup -fP --show "$RAW")"
sudo -n udevadm settle 2>/dev/null || true
# systemd-repart lays the partitions out in definition order: with the boot gap
# the loader hole is p1, so the ESP and root move up by one. Their filesystem
# types are checked by probing the nodes below, not through udev, which Kata
# does not run.
if [ -n "$BOOT_GAP" ]; then
  esp_dev="${LOOP}p2"
  root_dev="${LOOP}p3"
else
  esp_dev="${LOOP}p1"
  root_dev="${LOOP}p2"
fi
sudo -n udevadm settle 2>/dev/null || true
# Kata exposes the scanned partitions in sysfs but does not run udev to
# materialize their /dev nodes. Keep the validated nodes alive through mounts
# and bootc install, then release them before detaching the loop device.
# A Rockchip disk has the loader hole plus the ESP and root. The holder
# refuses a loop whose sysfs partitions are not exactly the ones it was given.
holder_args=("$LOOP")
if [ -n "$BOOT_GAP" ]; then
  holder_args+=("3")
fi
coproc LOOP_NODE_HOLDER { sudo -n python3 -u "$SCRIPT_DIR/hold-loop-partition-nodes.py" "${holder_args[@]}"; }
LOOP_NODE_HOLDER_PID=$!
LOOP_NODE_HOLDER_IN=${LOOP_NODE_HOLDER[1]}
LOOP_NODE_HOLDER_OUT=${LOOP_NODE_HOLDER[0]}
IFS= read -r loop_nodes_ready <&"$LOOP_NODE_HOLDER_OUT" || die "loop partition node holder failed"
[ "$loop_nodes_ready" = READY ] || die "loop partition node holder did not become ready"
if [ -n "$BOOT_GAP" ]; then
  [ "$(sudo -n blkid -p -s TYPE -o value "$esp_dev")" = vfat ] \
    || die "boot-gap disk: $esp_dev is not the FAT ESP"
  case "$(sudo -n blkid -p -s TYPE -o value "$root_dev")" in
    btrfs|ext4) ;;
    *) die "boot-gap disk: $root_dev is not the root filesystem" ;;
  esac
fi
if [ "$ROOTFS" = ext4 ] && [ "$COMPOSEFS" = yes ]; then
  # systemd-repart uses mkfs.ext4 defaults, which omit the filesystem-wide
  # verity feature. Without it bootc silently installs unverified composefs
  # objects, and the resulting image cannot perform a verified upgrade.
  sudo -n tune2fs -O verity "$root_dev"
  sudo -n tune2fs -l "$root_dev" | grep -Eq '^Filesystem features:.*[[:space:]]verity([[:space:]]|$)' \
    || die "ext4 root is missing its fs-verity filesystem feature"
fi
mkdir -p "$MNT"
if [ "$ROOTFS" = btrfs ]; then
  # Compress objects while bootc installs them, before a small board disk fills.
  # Level 3 reduces the installed immutable payload. This mount exists only
  # during assembly; the booted image still selects its runtime mount policy.
  # The inherited inode property also applies to objects created after reboot;
  # the installation mount option alone would only cover this mount's lifetime.
  sudo -n mount -o compress=zstd:3 "$root_dev" "$MNT"
  sudo -n btrfs property set "$MNT" compression zstd
else
  sudo -n mount "$root_dev" "$MNT"
fi
if [ "$BOOT_IS_ESP" = yes ]; then ESP_AT="$MNT/boot"; else ESP_AT="$MNT/boot/efi"; fi
sudo -n mkdir -p "$ESP_AT"
if [ "${TILO_DISK_FORCE_MTOOLS_ESP:-0}" != 1 ] \
   && sudo -n mount "$esp_dev" "$ESP_AT" 2>"$OUTDIR/.esp-mount.log"; then
  msg "2/4 mounted $root_dev -> $MNT, $esp_dev -> $ESP_AT"
else
  if [ "${TILO_DISK_FORCE_MTOOLS_ESP:-0}" != 1 ] \
     && grep -qw vfat /proc/filesystems 2>/dev/null; then
    die "mounting the vfat ESP $esp_dev at $ESP_AT failed even though this kernel
  advertises vfat support. Refusing to hide a disk or mount failure behind the
  userspace ESP fallback. See $OUTDIR/.esp-mount.log"
  fi
  command -v mcopy >/dev/null \
    || die "this kernel cannot mount vfat and mcopy is unavailable; install mtools"
  ESP_UUID="$(sudo -n blkid -s UUID -o value "$esp_dev")"
  [[ "$ESP_UUID" =~ ^[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}$ ]] \
    || die "could not read the formatted ESP UUID from $esp_dev"
  # bootc's empty-root verifier requires /boot (or /boot/efi) to be a mount
  # point and rejects every unrelated directory inside the target root. Keep
  # the bind source outside $MNT on a real host filesystem with a UUID. bootc
  # can inspect that mount while --boot-mount-spec below remains authoritative
  # for the real ESP UUID in the UKI arguments and installed fstab.
  esp_stage_parent="${TILO_DISK_ESP_STAGE_PARENT:-/var/tmp}"
  [ -d "$esp_stage_parent" ] \
    || die "ESP staging parent does not exist: $esp_stage_parent"
  ESP_STAGE="$(mktemp -d "$esp_stage_parent/tilo-esp-stage.XXXXXX")"
  chmod 0700 "$ESP_STAGE"
  case "$(readlink -f "$ESP_STAGE")/" in
    "$(readlink -f "$MNT")/"*) die "ESP staging directory must be outside the target root" ;;
  esac
  esp_stage_uuid="$(findmnt -n -o UUID --target "$ESP_STAGE")"
  [ -n "$esp_stage_uuid" ] \
    || die "ESP staging filesystem at $ESP_STAGE has no UUID; choose a real host filesystem with TILO_DISK_ESP_STAGE_PARENT"
  sudo -n mount --bind "$ESP_STAGE" "$ESP_AT" \
    || die "could not bind the ESP staging directory at $ESP_AT"
  sudo -n mount -o remount,bind,nosuid,nodev,noexec "$ESP_AT" \
    || die "could not apply safe options to the ESP staging bind mount"
  ESP_STAGE_BIND_MOUNTED=1
  ESP_STAGED=1
  msg "2/4 mounted $root_dev -> $MNT; staging ESP files at $ESP_AT for mtools"
fi

# --- install ------------------------------------------------------------------
# --generic-image is what keeps the install off the *build host's* firmware: it
# skips the NVRAM boot-entry write that would otherwise point this machine's UEFI
# at a disk image, and installs every bootloader type rather than only the one
# this host booted. bib passed it; a disk image always wants it.
install_args=(to-filesystem --generic-image --skip-fetch-check
  --target-imgref "$TARGET_IMGREF")
[ "$COMPOSEFS" = yes ] && install_args+=(--composefs-backend)
[ -n "$BOOTLOADER" ] && install_args+=(--bootloader "$BOOTLOADER")
[ "$ESP_STAGED" = 1 ] && [ "$BOOT_IS_ESP" = yes ] \
  && install_args+=(--boot-mount-spec "UUID=$ESP_UUID")
[ -n "$SRC_REF" ] && install_args+=(--source-imgref "$SRC_REF")
install_args+=("${KARGS[@]+"${KARGS[@]}"}")
[ "$DIRECT_SEALED_SOURCE" = 0 ] || install_args+=(--enforce-container-sigpolicy)
install_args+=("/run/tilo-install-target")

# Bind only the prepared disk target, not the job container's root filesystem.
# The recursive bind includes the ESP mounted below $MNT at /boot.
run_args=(--rm --privileged --pid=host
  --security-opt label=type:unconfined_t
  -v "$ROOT_CONTAINERS:/var/lib/containers"
  -v /dev:/dev -v "$MNT:/run/tilo-install-target:rbind")
# Bootc compares /run/udev with /proc/1/root/run/udev even without a udev
# database. Kata CI has no udev daemon, so create the parent-namespace path
# before Podman starts; otherwise that comparison fails with ENOENT. When a
# real database exists, project it so loop devices resolve during installation.
sudo -n mkdir -p /run/udev
host_udev_dir="${TILO_DISK_HOST_UDEV_DIR:-/run/udev}"
if [ -d "$host_udev_dir/data" ]; then
  host_udev_dir="$(readlink -f "$host_udev_dir")"
else
  host_udev_dir=/run/udev
fi
run_args+=(-v "$host_udev_dir:/run/udev:ro")
bootc_argv=(bootc)
if [ -n "$BOOTC_HELPER" ]; then
  run_args+=(-v "$BOOTC_HELPER:/run/tilo-installer-bootc:ro"
    --entrypoint /run/tilo-installer-bootc)
  bootc_argv=()
  msg "using explicit bootc helper $BOOTC_HELPER"
  if [ "$RESEAL_UKI" = 1 ]; then
    run_args+=(-e BOOTC_INSTALL_RESEAL_UKI=1)
  fi
fi
if [ -n "$SRC_REF" ]; then
  run_args+=(--network host
    -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro")
fi
if [ -n "$SOURCE_POLICY" ]; then
  run_args+=(-v "$SOURCE_POLICY:/etc/containers/policy.json:ro")
fi

msg "3/4 bootc install ${install_args[*]}"
if ! root_podman run "${run_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
       "$RUNREF" "${bootc_argv[@]}" install "${install_args[@]}" 2>&1 | tee "$LOG"; then
  # Report both the target and its sparse backing filesystem: either can
  # exhaust space, and the composefs object writer does not identify which.
  msg "failed install storage: target and raw-image backing filesystem"
  sudo -n df -h "$MNT" "$OUTDIR" >&2 || true
  sudo -n df -i "$MNT" "$OUTDIR" >&2 || true
  if [ "$ROOTFS" = btrfs ]; then
    sudo -n btrfs filesystem usage "$MNT" >&2 || true
  fi
  # The two failures that are specific to dropping bib get named, because
  # neither message says what to do about it.
  if grep -q 'Invalid splitstream content type' "$LOG"; then
    die "the composefs pull rejected the install source.
  That is containers-storage: the composefs backend can only pull from a registry
  (\`podman save | sudo podman load\` produces exactly this error). This script
  publishes to a local registry for that reason — check the push above succeeded.
  Full log: $LOG"
  fi
  want="$(composefs_expected_digest "$LOG")"
  if [ -n "$want" ]; then
    die "the UKI's sealed composefs digest is not the one install computed (bootc#2334).
  \`bootc container ukify\` and \`bootc install\` disagree on the digest of the same
  image, so the image has to be rebuilt with the digest install asked for sealed
  into the UKI, and installed again. The expected value is stable across that
  rebuild, so two passes converge (os/dev/composefs/seal.sh automates the loop).
    seal this digest: sha512:$want
  Full log: $LOG"
  fi
  die "bootc install failed; full log: $LOG"
fi

# A composefs repository whose objects carry no fs-verity still boots, but its
# first `bootc upgrade` fails with EIO ("lower file ... has no fs-verity
# digest"). bootc installs such a repository without a word when the kernel it
# runs on lacks fs-verity, so check every object here.
if [ "$COMPOSEFS" = yes ]; then
  sudo -n python3 "$SCRIPT_DIR/check-composefs-verity.py" "$MNT/composefs/objects" \
    || die "the installed composefs objects lack fs-verity: check $ROOTFS feature flags and kernel $(uname -r) support"
fi

# --- bib's customizations.user, replaced --------------------------------------
# useradd is run inside the image, against the installed system's /etc and /var
# bind-mounted in — so it uses the image's own shadow-utils configuration and
# lands the home directory on the target's /var/home, which is where a bootc
# system keeps it. Doing it with only /etc bound would write the passwd entry to
# the disk and the home directory into the discarded container.
if [ -n "$USERSPEC" ]; then
  IFS=: read -r u_name u_pass u_groups <<< "$USERSPEC"
  # The composefs backend deploys under /state/deploy/<digest>/ with the stateroot
  # at /state/os/<name>/; the ostree backend uses /ostree/deploy/<name>/. On both,
  # /etc is per-deployment and /var belongs to the stateroot — the deployment's own
  # `var` is only a symlink to it.
  t_etc="$(sudo -n sh -c "ls -d $MNT/state/deploy/*/etc $MNT/ostree/deploy/*/deploy/*/etc 2>/dev/null | head -1")"
  t_var="$(sudo -n sh -c "ls -d $MNT/state/os/*/var $MNT/ostree/deploy/*/var 2>/dev/null | head -1")"
  [ -n "$t_etc" ] || die "could not find the installed system's /etc under $MNT"
  [ -n "$t_var" ] || die "could not find the installed system's /var under $MNT"
  # bootc's finalize step leaves the target mounted read-only, on which useradd
  # fails as "cannot lock /etc/group; try again later" — which reads like a stale
  # lock rather than like a read-only filesystem.
  sudo -n mount -o remount,rw "$MNT"
  msg "creating user $u_name (groups: ${u_groups:-none}) in $t_etc"
  # $RUNREF, not $IMAGE: on the composefs path the image was never copied into
  # ROOT container storage — only pushed to the local registry, which is where
  # the install pulled it from — so root podman resolves the rootless-storage
  # name as a registry reference and fails "pinging container registry localhost".
  user_run_args=(--rm --privileged
    -v "$t_etc:/target-etc" -v "$t_var:/target-var")
  if [ -n "$SRC_REF" ]; then
    user_run_args+=(--network host
      -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro")
  fi
  root_podman run "${user_run_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
    "$RUNREF" \
    bash -c "set -e
      mount --bind /target-etc /etc
      mount --bind /target-var /var
      useradd -m ${u_groups:+-G '$u_groups'} '$u_name'
      printf '%s:%s\n' '$u_name' '$u_pass' | chpasswd
      # An always-on board image ships the session template and its marker;
      # its instance is the account created here.
      board_wants=/etc/systemd/system/multi-user.target.wants
      board_session=\"\$board_wants/tilo-board-headless-session@$u_name.service\"
      if [ -f /usr/share/tilo/board-headless-session ]; then
        install -d -m 0755 \"\$board_wants\"
        ln -sfn /usr/lib/systemd/system/tilo-board-headless-session@.service \"\$board_session\"
      fi
      # Label the new home. A home directory inherited from /var comes out
      # \`var_t\` rather than \`user_home_dir_t\`, which an SELinux-enforcing session
      # does not get to use.
      #
      # \`restorecon\` cannot do this: a container has no /sys/fs/selinux, so
      # is_selinux_enabled() is false and restorecon exits 0 having relabelled
      # nothing — silently, which is how this shipped mislabelled the first time.
      # \`setfiles\` given an explicit file_contexts works with SELinux inactive,
      # which is exactly the offline-relabel case it exists for.
      fc=/etc/selinux/targeted/contexts/files/file_contexts
      [ -r \"\$fc\" ] || { echo 'no file_contexts in the image; home left unlabelled' >&2; exit 1; }
      # useradd and chpasswd atomically replace these account files. With
      # SELinux inactive in the offline container, their new inodes otherwise
      # boot as unlabeled_t and break NSS in dbus-broker and greetd.
      for db in passwd group shadow gshadow; do
        [ -f \"/etc/\$db\" ] || { echo \"missing /etc/\$db after account creation\" >&2; exit 1; }
        setfiles -F \"\$fc\" \"/etc/\$db\"
      done
      setfiles -F \"\$fc\" /var/home
      if [ -L \"\$board_session\" ]; then
        setfiles -F \"\$fc\" \"\$board_session\"
      fi"
fi

# Exercise the same one-use account handoff as the graphical installer. This
# deliberately does not call useradd: the baked first-boot service validates
# and consumes the seed on the installed system, before greetd starts.
if [ -n "$FIRSTBOOT_SEED" ]; then
  t_etc="$(sudo -n sh -c "ls -d $MNT/state/deploy/*/etc $MNT/ostree/deploy/*/deploy/*/etc 2>/dev/null | head -1")"
  [ -n "$t_etc" ] || die "could not find the installed system's /etc under $MNT"
  sudo -n mount -o remount,rw "$MNT"
  sudo -n install -d -m 0755 "$t_etc/tilo" \
    "$t_etc/systemd/system/multi-user.target.wants"
  sudo -n install -m 0600 "$FIRSTBOOT_SEED" "$t_etc/tilo/install-user.conf"
  sudo -n ln -sfn /usr/lib/systemd/system/tilo-firstboot-user.service \
    "$t_etc/systemd/system/multi-user.target.wants/tilo-firstboot-user.service"
  msg "seeded $firstboot_user for first-boot account creation"
fi

# Validation access for the release gate, written into this disk copy only and
# listed in the log. The image, its UKI and any published disk never see it.
if [ -n "$ETC_OVERLAY" ]; then
  t_etc="$(sudo -n sh -c "ls -d $MNT/state/deploy/*/etc $MNT/ostree/deploy/*/deploy/*/etc 2>/dev/null | head -1")"
  [ -n "$t_etc" ] || die "could not find the installed system's /etc under $MNT"
  sudo -n mount -o remount,rw "$MNT"
  mapfile -t overlay_entries < <(cd "$ETC_OVERLAY" && find . \( -type f -o -type l \) -printf '%P\n' | LC_ALL=C sort)
  msg "adding validation access to this disk's /etc: ${overlay_entries[*]}"
  # Add the entries without touching the directories that already exist: a
  # `cp -a SRC/. DST/` also copies SRC's own mode, timestamps and SELinux
  # context onto DST, which left the deployment's /etc labelled
  # `unconfined_u:object_r:var_t` (the runner's work directory) instead of
  # `etc_t`, so confined services (ldconfig, accounts-daemon) could not use it.
  # Every directory the overlay lands in that the deployment already has:
  # /etc itself and each existing parent of an overlay entry. Their label,
  # mode and owner must come out exactly as they went in.
  existing_dirs=("$t_etc")
  while IFS= read -r dir; do
    sudo -n test -d "$t_etc/$dir" && existing_dirs+=("$t_etc/$dir")
  done < <(cd "$ETC_OVERLAY" && find . -mindepth 1 -type d -printf '%P\n' | LC_ALL=C sort)
  # The label is read as the xattr itself: a runner without SELinux enabled
  # still stores and copies it, and stat's %C would only print "?" there.
  dir_metadata() {
    sudo -n python3 - "${existing_dirs[@]}" <<'PY'
import os, sys
for path in sys.argv[1:]:
    st = os.stat(path)
    try:
        label = os.getxattr(path, "security.selinux").rstrip(b"\0").decode()
    except OSError:
        label = "-"
    print(f"{st.st_mode & 0o7777:o} {st.st_uid}:{st.st_gid} {label} {path}")
PY
  }
  before_overlay="$(dir_metadata)"
  sudo -n tar -C "$ETC_OVERLAY" --no-xattrs --no-selinux -cf - . \
    | sudo -n tar -C "$t_etc" --no-same-owner --no-overwrite-dir -xf -
  after_overlay="$(dir_metadata)"
  if [ "$before_overlay" != "$after_overlay" ]; then
    diff <(printf '%s\n' "$before_overlay") <(printf '%s\n' "$after_overlay") >&2 || true
    die "the validation overlay changed the label, mode or owner of an existing /etc directory"
  fi
  overlay_label_args=(--rm --privileged -v "$t_etc:/target-etc")
  if [ -n "$SRC_REF" ]; then
    overlay_label_args+=(--network host
      -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro")
  fi
  # Offline labelling with the image's own policy, as for the account above.
  root_podman run "${overlay_label_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
    "$RUNREF" bash -c '
      set -e
      fc=/etc/selinux/targeted/contexts/files/file_contexts
      test -r "$fc"
      cp "$fc" /tmp/file_contexts
      mount --bind /target-etc /etc
      for entry do setfiles -F /tmp/file_contexts "/etc/$entry"; done
    ' label "${overlay_entries[@]}" \
    || die "could not label the validation access files"
fi

# The transport is copied to writable /var only after bootc has installed the
# sealed live image. Its manifest and every blob are rechecked both before and
# after the copy; the guest still verifies the CI signature before using it.
if [ -n "$OFFLINE_TARGET_DIR" ]; then
  t_var="$(sudo -n sh -c "ls -d $MNT/state/os/*/var $MNT/ostree/deploy/*/var 2>/dev/null | head -1")"
  [ -n "$t_var" ] || die "could not find the installed live medium's /var"
  sudo -n mount -o remount,rw "$MNT"
  sudo -n install -d -m 0700 "$t_var/lib/tilo-installer"
  sudo -n python3 "$(dirname "${BASH_SOURCE[0]}")/installer-offline-target.py" \
    install "$OFFLINE_TARGET_DIR" "$t_var/lib/tilo-installer/target-oci" "${OFFLINE_TARGET_FLAGS[@]}" \
    || die "could not stage the verified offline target into the live medium"
  # The host runner may have no SELinux policy or restorecon at all. Use the
  # install image's own file_contexts through the same confined offline-label
  # method as first-boot account seeding above. The guest installer service
  # must be able to read these blobs while SELinux remains enforcing.
  label_args=(--rm --privileged -v "$t_var:/target-var")
  if [ -n "$SRC_REF" ]; then
    label_args+=(--network host
      -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro")
  fi
  root_podman run "${label_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
    "$RUNREF" bash -c '
      set -e
      mount --bind /target-var /var
      fc=/etc/selinux/targeted/contexts/files/file_contexts
      test -r "$fc"
      setfiles -F "$fc" /var/lib/tilo-installer/target-oci
    ' || die "could not apply the installer image SELinux labels to the offline target"
fi

# WSL kernels can omit vfat entirely. bootc can still finalize the boot tree in
# the root filesystem; copy that tree into the already-formatted ESP with
# mtools, verify every payload byte by copying it back, and only then remove the
# staging copy. Native Linux keeps the normal mounted-vfat path above.
if [ "$ESP_STAGED" = 1 ]; then
  shopt -s dotglob nullglob
  esp_entries=("$ESP_AT"/*)
  [ "${#esp_entries[@]}" -gt 0 ] || die "bootc produced no staged ESP payload at $ESP_AT"
  esp_payload_bytes="$(sudo -n du -sb "$ESP_AT" | awk '{print $1}')"
  [ "$esp_payload_bytes" -le $(( esp_mib * 1024 * 1024 )) ] \
    || die "staged ESP payload exceeds the formatted ${esp_mib} MiB partition"
  if sudo -n find "$ESP_AT" -mindepth 1 \( -type l -o ! -type d ! -type f \) -print -quit \
       | grep -q .; then
    die "staged ESP contains a symlink or special file, which mtools cannot preserve safely"
  fi
  rm -rf "$ESP_VERIFY"
  mkdir -p "$ESP_VERIFY"
  msg "copying finalized ESP payload to $esp_dev with mtools"
  sudo -n mcopy -s -p -m -i "$esp_dev" "${esp_entries[@]}" ::/
  sudo -n mcopy -s -p -m -i "$esp_dev" '::/*' "$ESP_VERIFY/"
  sudo -n chown -R "$(id -u):$(id -g)" "$ESP_VERIFY"
  (
    cd "$ESP_AT"
    find . -type f -print0 | sort -z | xargs -0 -r sha256sum
  ) >"$OUTDIR/.esp-source.sha256"
  (
    cd "$ESP_VERIFY"
    find . -type f -print0 | sort -z | xargs -0 -r sha256sum
  ) >"$OUTDIR/.esp-copied.sha256"
  cmp -s "$OUTDIR/.esp-source.sha256" "$OUTDIR/.esp-copied.sha256" \
    || die "mtools ESP verification failed; staged /boot was left intact"
  if [ "$BOOT_IS_ESP" = yes ]; then
    t_etc="$(sudo -n sh -c "ls -d $MNT/state/deploy/*/etc $MNT/ostree/deploy/*/deploy/*/etc 2>/dev/null | head -1")"
    [ -n "$t_etc" ] || die "could not find installed /etc to verify the ESP mount specification"
    [ "$(sudo -n awk -v source="UUID=$ESP_UUID" \
        '$1 == source && $2 == "/boot" { count++ } END { print count + 0 }' \
        "$t_etc/fstab")" -eq 1 ] \
      || die "installed fstab does not contain exactly one /boot entry for ESP UUID=$ESP_UUID"
  fi
  sudo -n mount -o remount,rw "$MNT"
  sudo -n find "$ESP_AT" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
  [ -z "$(sudo -n find "$ESP_AT" -mindepth 1 -print -quit)" ] \
    || die "could not remove the verified duplicate ESP payload from the root filesystem"
  sudo -n umount "$ESP_AT" \
    || die "verified ESP copy, but could not unmount its staging bind mount"
  ESP_STAGE_BIND_MOUNTED=0
  sudo -n rmdir "$ESP_STAGE" \
    || die "could not remove the empty ESP staging directory"
  ESP_STAGE=""
  msg "verified ESP payload and removed its temporary staging mount"
fi

# Only the optional dev-disk lane requests this. Read the actual installed
# ESP while the loop device is still attached, so the bundle cannot drift
# from the disk that will be published. The native CI runner mounts vfat;
# do not silently export from the temporary WSL ESP staging copy.
if [ -n "$DIRECT_BOOT_DIR" ]; then
  [ "$ESP_STAGED" = 0 ] || die "direct boot bundle needs the installed ESP mounted as vfat"
  esp_partuuid="$(sudo -n blkid -s PARTUUID -o value "$esp_dev")"
  [ -n "$esp_partuuid" ] || die "installed ESP has no GPT partition UUID"
  # A sealed image boots its one UKI; an unsealed one (the dev image) boots
  # the kernel and initrd its one BLS entry names, on the same ESP.
  if [ "$HAS_UKI" = yes ]; then
    mapfile -t installed_ukis < <(sudo -n find "$ESP_AT/EFI/Linux" -type f -name '*.efi' -print)
    [ "${#installed_ukis[@]}" -eq 1 ] \
      || die "direct boot bundle requires exactly one installed UKI; found ${#installed_ukis[@]}"
    sudo -n bash "$(dirname "${BASH_SOURCE[0]}")/export-direct-boot-bundle.sh" \
      "${installed_ukis[0]}" "$esp_partuuid" "$DIRECT_BOOT_DIR"
    bundle_source="UKI"
  else
    [ "$BOOT_IS_ESP" = yes ] \
      || die "direct boot bundle of an unsealed image needs its boot entries on the ESP (composefs layout)"
    mapfile -t installed_entries < <(sudo -n find "$ESP_AT/loader/entries" -maxdepth 1 -type f -name '*.conf' -print)
    [ "${#installed_entries[@]}" -eq 1 ] \
      || die "direct boot bundle requires exactly one installed boot entry; found ${#installed_entries[@]}"
    sudo -n bash "$(dirname "${BASH_SOURCE[0]}")/export-direct-boot-bundle.sh" --bls \
      "${installed_entries[0]}" "$ESP_AT" "$esp_partuuid" "$DIRECT_BOOT_DIR"
    bundle_source="boot entry"
  fi
  sudo -n chown -R "$(id -u):$(id -g)" "$DIRECT_BOOT_DIR"
  msg "exported installed $bundle_source direct boot bundle -> $DIRECT_BOOT_DIR"
fi

cleanup
trap - EXIT

if [ -n "$ROCKCHIP_LOADER" ]; then
  msg "installing the Rockchip loader into the leading gap of $RAW"
  python3 "$SCRIPT_DIR/install-rockchip-loader.py" \
    "$RAW" "$ROCKCHIP_LOADER/idbloader.img" "$ROCKCHIP_LOADER/u-boot.itb"
fi

# --- qcow2 --------------------------------------------------------------------
if [ "$KEEP_RAW" = 1 ]; then
  msg "4/4 leaving the raw image in place"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  # As for the qcow2 below: a VM that boots it must not wait on its writeback.
  inenv "sync -- '$RAW'"
  msg "DONE: $RAW"
else
  msg "4/4 qemu-img convert -> $QCOW"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  qcow_compress=${TILO_QCOW2_COMPRESS:-0}
  [[ $qcow_compress == 0 || $qcow_compress == 1 ]] || die 'TILO_QCOW2_COMPRESS must be 0 or 1'
  qcow_opts=''
  if [[ $qcow_compress == 1 ]]; then qcow_opts='-c -o compression_type=zstd'; fi
  # /usr/bin/qemu-img by path: an Android SDK on PATH shadows it with a
  # qemu-img 2.12 that writes an image the rest of the toolkit then reads.
  inenv "\$(test -x /usr/bin/qemu-img && echo /usr/bin/qemu-img || echo qemu-img) \
    convert -f raw -O qcow2 $qcow_opts '$RAW' '$QCOW'"
  rm -f "$RAW"
  # Write the image out now. Otherwise gigabytes of it are still dirty in the
  # host's page cache when a VM boots it, and the guest's first flush (the
  # sync() at switch-root) waits for that writeback: the release gate's boot
  # budget then measured the host's disk, not the OS (run 60770, 47 s).
  inenv "sync -- '$QCOW'"
  msg "DONE: $QCOW"
  ls -lh "$QCOW" >&2
fi
