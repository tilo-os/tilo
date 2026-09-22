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
#     --esp 2G                ESP size
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
#     --raw                   leave the raw image, skip the qcow2 conversion
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

source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

IMAGE=""; OUTDIR=""
COMPOSEFS=auto; BOOTLOADER=""; SIZE=""; ROOT_MIN=12G; ESP_SIZE=2G
RETAIN="${TILO_RETAINED_GENERATIONS:-3}"; UKI_BUDGET=160M
KARGS=(); USERSPEC=""; TARGET_IMGREF=""; SEALED_SOURCE_IMGREF=""
FIRSTBOOT_SEED=""
KEEP_RAW=0; DEFAULT_KARGS=yes

while [ $# -gt 0 ]; do
  case "$1" in
    --composefs) COMPOSEFS=yes ;;
    --no-composefs) COMPOSEFS=no ;;
    --bootloader) BOOTLOADER="$2"; shift ;;
    --size) SIZE="$2"; shift ;;
    --root-min) ROOT_MIN="$2"; shift ;;
    --esp) ESP_SIZE="$2"; shift ;;
    --retain) RETAIN="$2"; shift ;;
    --uki-budget) UKI_BUDGET="$2"; shift ;;
    --karg) KARGS+=("--karg" "$2"); shift ;;
    --no-default-kargs) DEFAULT_KARGS=no ;;
    --user) USERSPEC="$2"; shift ;;
    --target-imgref) TARGET_IMGREF="$2"; shift ;;
    --sealed-source-imgref) SEALED_SOURCE_IMGREF="$2"; shift ;;
    --firstboot-seed) FIRSTBOOT_SEED="$2"; shift ;;
    --raw) KEEP_RAW=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    -*) die "unknown option: $1" ;;
    *) if [ -z "$IMAGE" ]; then IMAGE="$1"; elif [ -z "$OUTDIR" ]; then OUTDIR="$1";
       else die "unexpected argument: $1"; fi ;;
  esac
  shift
done
[ -n "$IMAGE" ] && [ -n "$OUTDIR" ] || { sed -n '2,20p' "$0"; exit 1; }

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

if [ -z "$SEALED_SOURCE_IMGREF" ]; then
  podman image exists "$IMAGE" || die "$IMAGE is not in rootless podman storage"
else
  [ "$COMPOSEFS" = yes ] || die "--sealed-source-imgref requires --composefs"
  [[ "$IMAGE" =~ @sha256:[0-9a-f]{64}$ ]] \
    || die "--sealed-source-imgref requires digest-pinned IMAGE as the authoritative target-imgref"
  command -v skopeo >/dev/null || die "--sealed-source-imgref requires skopeo to verify the mirror digest"
  source_identity="$(skopeo inspect --tls-verify=false --format '{{.Name}} {{.Digest}}' \
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
# mounts the root read-only otherwise, and the two consoles because that is how
# a headless boot leaves a log the VM toolkit can read (os/dev/vm.sh serial).
# A sealed image cannot take kargs here at all, so for that layout they have to
# be in the image's own kargs.d — the message below says so.
if [ ${#KARGS[@]} -eq 0 ] && [ "$HAS_UKI" = no ] && [ "$DEFAULT_KARGS" = yes ]; then
  KARGS=(--karg rw --karg console=tty0 --karg console=ttyS0)
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
# Absolute, because the mount point is handed to the installer as /run/host$MNT —
# a relative path there silently becomes /run/hostos/... and fails as a missing
# target root.
OUTDIR="$(cd "$OUTDIR" && pwd)"
RAW="$OUTDIR/disk.raw"
QCOW="$OUTDIR/disk.qcow2"
MNT="$OUTDIR/.mnt"
DEFS="$OUTDIR/.repart.d"
LOG="$OUTDIR/.install.log"
ESP_VERIFY="$OUTDIR/.esp-verify"
ESP_STAGED=0
ESP_TMPFS_MOUNTED=0

cleanup() {
  if [ "$ESP_TMPFS_MOUNTED" = 1 ]; then
    sudo -n umount "$ESP_AT" 2>/dev/null || true
    ESP_TMPFS_MOUNTED=0
  fi
  sudo -n umount -R "$MNT" 2>/dev/null || true
  [ -n "${LOOP:-}" ] && sudo -n losetup -d "$LOOP" 2>/dev/null || true
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
start_registry() {
  local want="${TILO_DISK_REG_PORT:-5000}" port
  # An already-present registry container of ours is authoritative: reuse it on
  # whatever port it was published on.
  if podman container exists tilo-disk-registry 2>/dev/null; then
    [ "$(podman inspect -f '{{.State.Running}}' tilo-disk-registry)" = true ] \
      || podman start tilo-disk-registry >/dev/null
    port="$(podman port tilo-disk-registry 5000/tcp 2>/dev/null | head -1)"
    port="${port##*:}"
    [ -n "$port" ] || port="$want"
    echo "localhost:$port"; return
  fi
  # Someone else's registry already listening (the composefs spike leaves one on
  # :5000) is fine to push to — one local registry is as good as another.
  if curl -sf "http://localhost:$want/v2/" >/dev/null 2>&1; then
    msg "reusing the registry already listening on :$want" >&2
    echo "localhost:$want"; return
  fi
  for port in $(seq "$want" $(( want + 9 ))); do
    podman run -d --name tilo-disk-registry -p "$port:5000" \
      docker.io/library/registry:2 >/dev/null 2>&1 || {
        podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
        continue
      }
    for _ in $(seq 1 30); do
      curl -sf "http://localhost:$port/v2/" >/dev/null 2>&1 && break
      sleep 1
    done
    if curl -sf "http://localhost:$port/v2/" >/dev/null 2>&1; then
      msg "started a registry on :$port for the install source" >&2
      echo "localhost:$port"; return
    fi
    podman rm -f tilo-disk-registry >/dev/null 2>&1 || true
  done
  return 1
}

if [ -n "$SEALED_SOURCE_IMGREF" ]; then
  RUNREF="${SEALED_SOURCE_IMGREF#docker://}"
  SRC_REF="docker://$RUNREF"
  REG="${RUNREF%%/*}"
  TLS_ARGS=(--tls-verify=false)
  REGCONF="$OUTDIR/.registries.conf"
  printf '[[registry]]\nlocation = "%s"\ninsecure = true\n' "$REG" > "$REGCONF"
  # The image's runtime policy correctly trusts its signed authoritative
  # repository, not an unsigned transport mirror. bootc evaluates the running
  # mirror reference against that policy before install, so give this one
  # invocation a policy that rejects everything except the digest-verified
  # mirror repository. This file is bind-mounted over the running container's
  # policy only; the installed system retains the policy shipped in the image.
  SOURCE_POLICY="$OUTDIR/.source-policy.json"
  python3 - "$SOURCE_POLICY" "$source_repository" <<'PY'
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
  TLS_ARGS=(--tls-verify=false)
  REGCONF="$OUTDIR/.registries.conf"
  printf '[[registry]]\nlocation = "%s"\ninsecure = true\n' "$REG" > "$REGCONF"
elif ! sudo -n podman image exists "$IMAGE" 2>/dev/null; then
  msg "transfer $IMAGE into root container storage"
  podman save "$IMAGE" | sudo -n podman load
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
printf '[Partition]\nType=7a2e8d75-3f01-4e5a-b2c9-51f6ed90b8ab\nLabel=Tilo root\nFormat=btrfs\nSizeMinBytes=%s\nGrowFileSystem=yes\n' \
  "$ROOT_MIN" > "$DEFS/02-root.conf"

msg "1/4 systemd-repart -> $RAW (ESP $ESP_SIZE + btrfs root >= $ROOT_MIN, disk $SIZE)"
rm -f "$RAW"
truncate -s "$SIZE" "$RAW"
sudo -n systemd-repart --empty=force --dry-run=no --discard=no --offline=false \
  --definitions="$DEFS" --size="$SIZE" "$RAW"

LOOP="$(sudo -n losetup -fP --show "$RAW")"
sudo -n udevadm settle 2>/dev/null || true
mkdir -p "$MNT"
sudo -n mount "${LOOP}p2" "$MNT"
if [ "$BOOT_IS_ESP" = yes ]; then ESP_AT="$MNT/boot"; else ESP_AT="$MNT/boot/efi"; fi
sudo -n mkdir -p "$ESP_AT"
if [ "${TILO_DISK_FORCE_MTOOLS_ESP:-0}" != 1 ] \
   && sudo -n mount "${LOOP}p1" "$ESP_AT" 2>"$OUTDIR/.esp-mount.log"; then
  msg "2/4 mounted ${LOOP}p2 -> $MNT, ${LOOP}p1 -> $ESP_AT"
else
  if [ "${TILO_DISK_FORCE_MTOOLS_ESP:-0}" != 1 ] \
     && grep -qw vfat /proc/filesystems 2>/dev/null; then
    die "mounting the vfat ESP ${LOOP}p1 at $ESP_AT failed even though this kernel
  advertises vfat support. Refusing to hide a disk or mount failure behind the
  userspace ESP fallback. See $OUTDIR/.esp-mount.log"
  fi
  command -v mcopy >/dev/null \
    || die "this kernel cannot mount vfat and mcopy is unavailable; install mtools"
  # bootc requires /boot (or /boot/efi) to be a mount point and rejects an
  # empty directory in the target root. A bounded tmpfs supplies that mount on
  # kernels which cannot mount vfat; its contents are copied and verified into
  # the real ESP below, then the tmpfs is explicitly unmounted.
  sudo -n mount -t tmpfs \
    -o "size=${esp_mib}M,mode=0755,nosuid,nodev,noexec" \
    tilo-esp-stage "$ESP_AT" \
    || die "could not mount the bounded ESP staging tmpfs at $ESP_AT"
  ESP_TMPFS_MOUNTED=1
  ESP_STAGED=1
  msg "2/4 mounted ${LOOP}p2 -> $MNT; staging ESP files at $ESP_AT for mtools"
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
[ -n "$SRC_REF" ] && install_args+=(--source-imgref "$SRC_REF")
install_args+=("${KARGS[@]+"${KARGS[@]}"}")
install_args+=("/run/host$MNT")

run_args=(--rm --privileged --pid=host
  --security-opt label=type:unconfined_t
  -v /var/lib/containers:/var/lib/containers
  -v /dev:/dev -v /:/run/host)
if [ -n "$SRC_REF" ]; then
  run_args+=(--network host
    -v "$REGCONF:/etc/containers/registries.conf.d/98-local.conf:ro")
fi
if [ -n "$SOURCE_POLICY" ]; then
  run_args+=(-v "$SOURCE_POLICY:/etc/containers/policy.json:ro")
fi

msg "3/4 bootc install ${install_args[*]}"
if ! sudo -n podman run "${run_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
       "$RUNREF" bootc install "${install_args[@]}" 2>&1 | tee "$LOG"; then
  # The two failures that are specific to dropping bib get named, because
  # neither message says what to do about it.
  if grep -q 'Invalid splitstream content type' "$LOG"; then
    die "the composefs pull rejected the install source.
  That is containers-storage: the composefs backend can only pull from a registry
  (\`podman save | sudo podman load\` produces exactly this error). This script
  publishes to a local registry for that reason — check the push above succeeded.
  Full log: $LOG"
  fi
  want="$(sed -n "s/.*should be sha512:\([0-9a-f]\{128\}\).*/\1/p" "$LOG" | tail -1)"
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
  sudo -n podman run "${user_run_args[@]}" "${TLS_ARGS[@]+"${TLS_ARGS[@]}"}" \
    "$RUNREF" \
    bash -c "set -e
      mount --bind /target-etc /etc
      mount --bind /target-var /var
      useradd -m ${u_groups:+-G '$u_groups'} '$u_name'
      printf '%s:%s\n' '$u_name' '$u_pass' | chpasswd
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
      setfiles -F \"\$fc\" /var/home"
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

# WSL kernels can omit vfat entirely. bootc can still finalize the boot tree in
# the root filesystem; copy that tree into the already-formatted ESP with
# mtools, verify every payload byte by copying it back, and only then remove the
# staging copy. Native Linux keeps the normal mounted-vfat path above.
if [ "$ESP_STAGED" = 1 ]; then
  shopt -s dotglob nullglob
  esp_entries=("$ESP_AT"/*)
  [ "${#esp_entries[@]}" -gt 0 ] || die "bootc produced no staged ESP payload at $ESP_AT"
  if sudo -n find "$ESP_AT" -mindepth 1 \( -type l -o ! -type d ! -type f \) -print -quit \
       | grep -q .; then
    die "staged ESP contains a symlink or special file, which mtools cannot preserve safely"
  fi
  rm -rf "$ESP_VERIFY"
  mkdir -p "$ESP_VERIFY"
  msg "copying finalized ESP payload to ${LOOP}p1 with mtools"
  sudo -n mcopy -s -p -m -i "${LOOP}p1" "${esp_entries[@]}" ::/
  sudo -n mcopy -s -p -m -i "${LOOP}p1" '::/*' "$ESP_VERIFY/"
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
  sudo -n mount -o remount,rw "$MNT"
  sudo -n find "$ESP_AT" -mindepth 1 -maxdepth 1 -exec rm -rf -- {} +
  [ -z "$(sudo -n find "$ESP_AT" -mindepth 1 -print -quit)" ] \
    || die "could not remove the verified duplicate ESP payload from the root filesystem"
  sudo -n umount "$ESP_AT" \
    || die "verified ESP copy, but could not unmount its staging tmpfs"
  ESP_TMPFS_MOUNTED=0
  msg "verified ESP payload and removed its temporary staging mount"
fi

cleanup
trap - EXIT

# --- qcow2 --------------------------------------------------------------------
if [ "$KEEP_RAW" = 1 ]; then
  msg "4/4 leaving the raw image in place"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  msg "DONE: $RAW"
else
  msg "4/4 qemu-img convert -> $QCOW"
  sudo -n chown "$(id -u):$(id -g)" "$RAW"
  # /usr/bin/qemu-img by path: an Android SDK on PATH shadows it with a
  # qemu-img 2.12 that writes an image the rest of the toolkit then reads.
  inenv "\$(test -x /usr/bin/qemu-img && echo /usr/bin/qemu-img || echo qemu-img) \
    convert -f raw -O qcow2 '$RAW' '$QCOW'"
  rm -f "$RAW"
  msg "DONE: $QCOW"
  ls -lh "$QCOW" >&2
fi
