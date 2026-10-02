#!/bin/bash
# Build the Tilo self-install installer: a bootable live qcow2 that runs
# the graphical installer (installer/, binary tilo-installer) under
# cage and installs the booted image onto a target disk.
#
#   ./os/dev/build-image.sh                 # build + load localhost/tilo first
#   ./os/dev/build-custom-installer.sh      # -> os/output/installer/qcow2/disk.qcow2
#   ./os/dev/vm.sh boot-installer --gl      # boot it against a blank /dev/vdb
#
# Pipeline (mirrors build-image.sh):
#   1. cargo build --release -p installer (inside the build distrobox), then
#      stage target/release/tilo-installer -> os/tilo-installer.
#      (--stub-ui skips cargo and writes a placeholder tilo-installer so you can
#      test the OS plumbing — session-select, sudoers, units — without the UI.)
#   2. rootless `podman build` of localhost/tilo-installer (FROM localhost/tilo).
#      tilo.install is baked in here, as a kargs.d drop-in.
#   3. os/dev/disk-image.sh -> os/output/installer/qcow2/disk.qcow2.
#
# Flags: --stub-ui (no cargo; placeholder UI)  --no-cargo (reuse os/tilo-installer)
#        --reuse-image-installer (inherit the already-built native installer)
#        --image-only (stop at the unsigned installer layer, before disk/signing)
#        --dev-login (local diagnostics only; requires TILO_INSTALLER_DEV_PASSWORD)
#        --compact-ext4 (VM-only validation: fixed 40 GiB ext4 live root)
# Environment: TILO_INSTALLER_ROOTFS=btrfs|ext4 selects the full-size live root.
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# WSL may allow registry access on the host while Podman's private build
# namespace cannot reach Fedora's package mirrors. Keep the normal isolated
# network by default and allow an explicit host-network build in that case.
build_network_args=()
case "${TILO_PODMAN_BUILD_NETWORK:-default}" in
  default) ;;
  host) build_network_args=(--network=host) ;;
  *) die "TILO_PODMAN_BUILD_NETWORK must be default or host" ;;
esac

# Several native build tools ignore Cargo's target directory and create
# assembly/object scratch files through libc's TMPDIR.  Keep those files with
# the installer output as well: shared /tmp commonly has a tight per-user quota
# on developer and CI hosts, and a full /tmp must not break an otherwise
# reproducible installer build.
: "${TMPDIR:=${TILO_INSTALLER_OUTPUT}/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

STUB_UI=0
DO_CARGO=1
REUSE_IMAGE_INSTALLER=0
IMAGE_ONLY=0
BINARY_MODES=0
DEV_LOGIN=0
COMPACT_EXT4=0
for a in "$@"; do
  case "$a" in
    --stub-ui) STUB_UI=1; DO_CARGO=0; BINARY_MODES=$((BINARY_MODES + 1)) ;;
    --no-cargo) DO_CARGO=0; BINARY_MODES=$((BINARY_MODES + 1)) ;;
    --reuse-image-installer) REUSE_IMAGE_INSTALLER=1; DO_CARGO=0; BINARY_MODES=$((BINARY_MODES + 1)) ;;
    --image-only) IMAGE_ONLY=1 ;;
    --dev-login) DEV_LOGIN=1 ;;
    --compact-ext4) COMPACT_EXT4=1 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) die "unknown arg: $a" ;;
  esac
done

[ "$BINARY_MODES" -le 1 ] || die "choose only one installer binary source"
case "${TILO_INSTALLER_ROOTFS:-btrfs}" in
  btrfs|ext4) ;;
  *) die "TILO_INSTALLER_ROOTFS must be btrfs or ext4" ;;
esac
[ "$COMPACT_EXT4" = 0 ] || [ "$IMAGE_ONLY" = 0 ] \
  || die "--compact-ext4 requires disk creation; remove --image-only"

OFFLINE_TARGET_DIR="${TILO_INSTALLER_OFFLINE_TARGET_DIR:-}"
if [ -n "$OFFLINE_TARGET_DIR" ]; then
  [ "$COMPACT_EXT4" = 0 ] || die "--compact-ext4 cannot hold an offline target image"
  python3 "$TILO_OS_DIR/dev/installer-offline-target.py" verify "$OFFLINE_TARGET_DIR" >/dev/null \
    || die "TILO_INSTALLER_OFFLINE_TARGET_DIR is not a verified OCI transport"
fi

if [ "$DEV_LOGIN" = 1 ] && [ -z "${TILO_INSTALLER_DEV_PASSWORD:-}" ]; then
  die "--dev-login requires a non-empty TILO_INSTALLER_DEV_PASSWORD"
fi

# The selected base must already exist in ROOTLESS podman storage. Pass that
# exact reference through to the Containerfile so a model-specific build cannot
# silently layer its installer on a different localhost/tilo:latest image.
podman image exists "$TILO_IMAGE" \
  || die "$TILO_IMAGE not in rootless podman storage — run ./os/dev/build-image.sh first"

if [ "$REUSE_IMAGE_INSTALLER" = 1 ]; then
  msg "1/3 inheriting /usr/bin/tilo-installer from the selected base (no Cargo build)"
elif [ "$STUB_UI" = 1 ]; then
  msg "1/3 writing stub tilo-installer (--stub-ui: OS-plumbing test, no UI)"
  cat > "$TILO_OS_DIR/tilo-installer" <<'STUB'
#!/bin/bash
# Placeholder Tilo installer (built with --stub-ui). The real UI is omitted;
# this exists only to exercise the OS plumbing (session-select, sudoers,
# first-boot units). It stays alive so cage keeps the session up.
echo "tilo-installer STUB (--stub-ui): UI omitted; OS plumbing test only."
echo "kernel cmdline: $(cat /proc/cmdline 2>/dev/null)"
exec sleep infinity
STUB
  chmod 755 "$TILO_OS_DIR/tilo-installer"
elif [ "$DO_CARGO" = 1 ]; then
  msg "1/3 cargo build --release -p installer in ${TILO_DISTROBOX:-host}"
  inenv "cd '$TILO_REPO' && cargo build --release -p installer"
  REL="$TILO_REPO/target/release"
  [ -f "$REL/tilo-installer" ] || die "missing built binary: $REL/tilo-installer"
  cp -f "$REL/tilo-installer" "$TILO_OS_DIR/tilo-installer"
  msg "   staged tilo-installer ($(du -h "$REL/tilo-installer" | cut -f1))"
else
  msg "1/3 skipping cargo (--no-cargo); reusing $TILO_OS_DIR/tilo-installer"
  [ -f "$TILO_OS_DIR/tilo-installer" ] || die "no $TILO_OS_DIR/tilo-installer present (drop --no-cargo)"
fi

msg "2/3 podman build $TILO_INSTALLER_IMAGE (rootless)"
# Assemble one exact rootfs overlay so the Containerfile needs a single COPY
# layer. On overlay-backed bootc images, committing every tiny file as its own
# layer repeatedly scans the multi-gigabyte base and turns an iterative
# installer build into an hour-long job.
INSTALLER_SCRATCH="$TMPDIR"
INSTALLER_CONTEXT="$(mktemp -d "$INSTALLER_SCRATCH/tilo-installer-context.XXXXXX")"
cleanup_installer_context() { rm -rf "$INSTALLER_CONTEXT"; }
trap cleanup_installer_context EXIT
mkdir -p "$INSTALLER_CONTEXT/rootfs/usr" \
         "$INSTALLER_CONTEXT/rootfs/etc/greetd" \
         "$INSTALLER_CONTEXT/rootfs/usr/share/tilo"
cp -a "$TILO_OS_DIR/usr/." "$INSTALLER_CONTEXT/rootfs/usr/"
# `bootc install to-filesystem` otherwise picks the first ESP on the backing
# disk, which can be Windows' ESP rather than the ESP mounted at /target/boot/efi.
# Build the pinned corrected bootc once per source revision and put it in the
# existing single installer overlay layer. The installed OS never inherits it.
alongside_bootc_key="$(cat \
  "$TILO_OS_DIR/Containerfile.bootc-alongside-helper" \
  "$TILO_OS_DIR/external-assets/bootc/0001-probe-missing-partition-metadata.patch" \
  "$TILO_OS_DIR/external-assets/bootc-install-reseal/0002-select-mounted-esp.patch" \
  | sha256sum | cut -c1-16)"
alongside_bootc_ref="localhost/tilo-bootc-alongside-helper:${alongside_bootc_key}"
if ! podman image exists "$alongside_bootc_ref"; then
  podman build "${build_network_args[@]}" -f "$TILO_OS_DIR/Containerfile.bootc-alongside-helper" \
    -t "$alongside_bootc_ref" "$TILO_REPO"
fi
(
  alongside_bootc_container="$(podman create "$alongside_bootc_ref" /bootc)"
  trap 'podman rm -f "$alongside_bootc_container" >/dev/null 2>&1 || true' EXIT
  install -d -m 0755 "$INSTALLER_CONTEXT/rootfs/usr/libexec"
  podman cp "$alongside_bootc_container:/bootc" \
    "$INSTALLER_CONTEXT/rootfs/usr/libexec/tilo-bootc-alongside"
)
chmod 0755 "$INSTALLER_CONTEXT/rootfs/usr/libexec/tilo-bootc-alongside"
if [ "${TILO_UNVERIFIED_VM_INSTALL:-0}" = 1 ]; then
  [ -z "$OFFLINE_TARGET_DIR" ] && [ "${TILO_REQUIRE_PCR_PREDICTION:-0}" = 0 ] \
    || die "unverified VM install is only for a disposable self-install medium"
  install -Dm0444 /dev/null \
    "$INSTALLER_CONTEXT/rootfs/usr/share/tilo/unverified-vm-install"
fi
if [ "$REUSE_IMAGE_INSTALLER" = 0 ]; then
  install -Dm0755 "$TILO_OS_DIR/tilo-installer" \
    "$INSTALLER_CONTEXT/rootfs/usr/bin/tilo-installer"
else
  # Do not let a stale overlay copy replace the selected base binary.
  rm -f "$INSTALLER_CONTEXT/rootfs/usr/bin/tilo-installer"
fi
install -Dm0755 "$TILO_OS_DIR/dev/uki-cmdline.py" \
  "$INSTALLER_CONTEXT/rootfs/usr/libexec/tilo-uki-cmdline"
install -Dm0644 "$TILO_OS_DIR/etc/greetd/config.toml" \
  "$INSTALLER_CONTEXT/rootfs/usr/share/tilo/greetd-config.toml"
install -Dm0644 "$TILO_OS_DIR/etc/greetd/config.installer.toml" \
  "$INSTALLER_CONTEXT/rootfs/etc/greetd/config.toml"
if [ -n "$OFFLINE_TARGET_DIR" ]; then
  # The guest verifies the signed target without network access. This is the
  # architecture-pinned Cosign binary already authenticated by the CI asset
  # manifest, copied into the sealed installer layer (never from host PATH).
  cosign_asset="$TILO_REPO/os/external-assets/cache/cosign-v2.5.2-linux-arm64"
  [ -f "$cosign_asset" ] && [ ! -L "$cosign_asset" ] \
    || die "offline target requires the pinned ARM64 Cosign asset"
  printf '%s  %s\n' \
    2cbcea1873ad76274c3f241ef175d204654e3aac3e73e6ec4504e5227015cb0 "$cosign_asset" \
    | sha256sum -c - >/dev/null \
    || die "offline target Cosign asset checksum differs"
  install -Dm0755 "$cosign_asset" \
    "$INSTALLER_CONTEXT/rootfs/usr/libexec/tilo-cosign"
fi
podman build "${build_network_args[@]}" \
  --build-arg "TILO_BASE_IMAGE=$TILO_IMAGE" \
  --build-arg "TILO_INSTALLER_DEV_SSH=$DEV_LOGIN" \
  -t "$TILO_INSTALLER_IMAGE" -f "$TILO_OS_DIR/Containerfile.installer" "$INSTALLER_CONTEXT"
cleanup_installer_context
trap - EXIT

if [ "$IMAGE_ONLY" = 1 ]; then
  msg "DONE: intermediate installer image $TILO_INSTALLER_IMAGE"
  msg "The new rootfs and modified live/target UKIs require sealing and signing before disk creation."
  exit 0
fi

msg "3/3 disk image -> installer qcow2"
# The live root holds both the booted composefs tree and a second unpacked image
# in /var/lib/containers (`bootc image copy-to-storage`) before `bootc install`.
# Size it from the actual image rather than a stale guess. Three times the OCI
# image size covers both trees plus overlay metadata; 40 GiB is the tested floor
# for the full Windows-compatibility payload. The pristine qcow2 remains sparse.
#
# `tilo.install` is not passed here — it lives in the image, as
# /usr/lib/bootc/kargs.d/50-installer.toml (see os/Containerfile.installer).
image_bytes="$(podman image inspect "$TILO_INSTALLER_IMAGE" --format '{{.Size}}')"
[[ "$image_bytes" =~ ^[0-9]+$ ]] && [ "$image_bytes" -gt 0 ] \
  || die "podman returned an invalid installer image size: $image_bytes"
gib=$((1024 * 1024 * 1024))
root_gib=$(((image_bytes * 3 + gib - 1) / gib))
[ "$root_gib" -ge 40 ] || root_gib=40
if [ -n "$OFFLINE_TARGET_DIR" ]; then
  # The target OCI archive lives in /var of the live medium, and podman load
  # materializes a second copy into container storage during installation.
  # Reserve both from the actual transported bytes before partitioning.
  offline_bytes="$(du -sb "$OFFLINE_TARGET_DIR" | awk '{print $1}')"
  offline_gib=$(((offline_bytes + gib - 1) / gib))
  root_gib=$((root_gib + offline_gib * 2 + 4))
  msg "   reserving 2x ${offline_gib}G for OCI target transport/import plus 4G slack in live /var"
fi
rootfs="${TILO_INSTALLER_ROOTFS:-btrfs}"
if [ "$COMPACT_EXT4" = 1 ]; then
  [ "$image_bytes" -le $((18 * gib)) ] \
    || die "--compact-ext4 needs 2x OCI image bytes plus 4 GiB slack within 40 GiB; image is $(numfmt --to=iec-i --suffix=B "$image_bytes"). Use the default growing layout."
  root_gib=40
  rootfs=ext4
  msg "   experimental compact VM live root: ext4 40G (OCI image $(numfmt --to=iec-i --suffix=B "$image_bytes"), 2x estimate + 4G slack)"
else
  msg "   live root ${root_gib}G (OCI image $(numfmt --to=iec-i --suffix=B "$image_bytes"), 3x + 40G floor)"
fi
rm -rf "$TILO_INSTALLER_OUTPUT/qcow2" "$TILO_INSTALLER_OUTPUT"/manifest* 2>/dev/null || true
DISK_ARGS=(
  "$TILO_INSTALLER_IMAGE"
  "$(dirname "$TILO_INSTALLER_DISK")"
  --root-min "${root_gib}G"
  --rootfs "$rootfs"
)
[ -z "$OFFLINE_TARGET_DIR" ] || DISK_ARGS+=(--offline-target-dir "$OFFLINE_TARGET_DIR")
[ "$COMPACT_EXT4" = 0 ] || DISK_ARGS+=(--root-max 40G)
if [ "$DEV_LOGIN" = 1 ]; then
  DISK_ARGS+=(--user "quickuser:${TILO_INSTALLER_DEV_PASSWORD}:wheel")
fi
INSTALL_LOG="$(dirname "$TILO_INSTALLER_DISK")/.install.log"
if ! "$TILO_OS_DIR/dev/disk-image.sh" "${DISK_ARGS[@]}"; then
  want="$(composefs_expected_digest "$INSTALL_LOG")"
  [ -n "$want" ] || die "the installer disk-image step failed for a reason other than the
  composefs seal; see the message above and $INSTALL_LOG"
  had="$(composefs_sealed_digest "$INSTALL_LOG")"
  msg "bootc#2334: the live UKI sealed sha512:${had:0:32}… but install wants sha512:${want:0:32}…"
  msg "pass 2: rewriting that digest into the live UKI and retrying"
  "$TILO_OS_DIR/dev/reseal-image.sh" "$TILO_INSTALLER_IMAGE" "$want" \
    || die "could not rewrite the live installer's sealed composefs digest"
  if ! "$TILO_OS_DIR/dev/disk-image.sh" "${DISK_ARGS[@]}"; then
    want2="$(composefs_expected_digest "$INSTALL_LOG")"
    if [ -n "$want2" ] && [ "$want2" != "$want" ]; then
      die "the digest install expects MOVED between passes (${want:0:16}… -> ${want2:0:16}…),
  even though only the UKI under /boot changed. Sealing is circular on this bootc
  version and requires an upstream fix."
    fi
    die "the second installer image pass failed too; see above and $INSTALL_LOG"
  fi
  msg "live installer seal closed in two passes"
fi

# PCR prediction signatures are produced only after the composefs reseal has
# converged, so both the TPM policy and detached signature cover the exact UKI
# that release media will carry. The signer is a protected runner-side broker
# executable; no private-key bytes enter this process, checkout, or image.
require_prediction="${TILO_REQUIRE_PCR_PREDICTION:-0}"
allow_local_prediction="${TILO_ALLOW_LOCAL_PCR_SIGNING:-0}"
case "$require_prediction:$allow_local_prediction" in
  0:0|0:1|1:0|1:1) ;;
  *) die "TILO_REQUIRE_PCR_PREDICTION and TILO_ALLOW_LOCAL_PCR_SIGNING must be 0 or 1" ;;
esac
if [ "$require_prediction" = 1 ] || [ "$allow_local_prediction" = 1 ]; then
  [ -n "${TILO_PCR_PREDICTION_SIGNER:-}" ] \
    || die "PCR prediction signing was requested but no TILO_PCR_PREDICTION_SIGNER is configured"
  "$TILO_OS_DIR/dev/sign-target-prediction.sh" \
    "$TILO_INSTALLER_IMAGE" "$TILO_PCR_PREDICTION_SIGNER"
  rm -rf "$TILO_INSTALLER_OUTPUT/qcow2" "$TILO_INSTALLER_OUTPUT"/manifest* 2>/dev/null || true
  if ! "$TILO_OS_DIR/dev/disk-image.sh" "${DISK_ARGS[@]}"; then
    die "adding /boot-only signed PCR prediction sidecars changed or invalidated the converged installer image"
  fi
fi

# Bind the completed qcow2 to the final installer image after all resealing and
# prediction sidecars. This is build provenance, separate from boot acceptance.
python3 "$TILO_REPO/apps/imager/packaging/media-provenance.py" create \
  "$TILO_INSTALLER_IMAGE" "$TILO_INSTALLER_DISK"

msg "DONE: $TILO_INSTALLER_DISK"
ls -lh "$TILO_INSTALLER_DISK"
