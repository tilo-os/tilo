#!/bin/bash
# Build the qOS CUSTOM (self-install) installer: a bootable live qcow2 that runs
# the qOS-branded graphical installer (installer/, binary qos-installer) under
# cage and installs the booted image onto a target disk. It is the only installer
# path; the Anaconda ISO it used to sit beside has been retired.
#
#   ./os/dev/build-image.sh                 # build + load localhost/qos first
#   ./os/dev/build-custom-installer.sh      # -> os/output/installer/qcow2/disk.qcow2
#   ./os/dev/vm.sh boot-installer --gl      # boot it against a blank /dev/vdb
#
# Pipeline (mirrors build-image.sh):
#   1. cargo build --release -p installer (inside the build distrobox), then
#      stage target/release/qos-installer -> os/qos-installer.
#      (--stub-ui skips cargo and writes a placeholder qos-installer so you can
#      test the OS plumbing — session-select, sudoers, units — without the UI.)
#   2. rootless `podman build` of localhost/qos-installer (FROM localhost/qos).
#      qos.install is baked in here, as a kargs.d drop-in.
#   3. os/dev/disk-image.sh -> os/output/installer/qcow2/disk.qcow2.
#
# Flags: --stub-ui (no cargo; placeholder UI)  --no-cargo (reuse os/qos-installer)
#        --dev-login (local diagnostics only; requires QOS_INSTALLER_DEV_PASSWORD)
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

# Several native build tools ignore Cargo's target directory and create
# assembly/object scratch files through libc's TMPDIR.  Keep those files with
# the installer output as well: shared /tmp commonly has a tight per-user quota
# on developer and CI hosts, and a full /tmp must not break an otherwise
# reproducible installer build.
: "${TMPDIR:=${QOS_INSTALLER_OUTPUT}/.tmp}"
mkdir -p "$TMPDIR"
export TMPDIR

STUB_UI=0
DO_CARGO=1
DEV_LOGIN=0
for a in "$@"; do
  case "$a" in
    --stub-ui) STUB_UI=1; DO_CARGO=0 ;;
    --no-cargo) DO_CARGO=0 ;;
    --dev-login) DEV_LOGIN=1 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) die "unknown arg: $a" ;;
  esac
done

if [ "$DEV_LOGIN" = 1 ] && [ -z "${QOS_INSTALLER_DEV_PASSWORD:-}" ]; then
  die "--dev-login requires a non-empty QOS_INSTALLER_DEV_PASSWORD"
fi

# Containerfile.installer is `FROM localhost/qos:latest`, so the base desktop
# image must already exist in ROOTLESS podman storage (the build is rootless).
podman image exists "$QOS_IMAGE" \
  || die "$QOS_IMAGE not in rootless podman storage — run ./os/dev/build-image.sh first"

if [ "$STUB_UI" = 1 ]; then
  msg "1/3 writing stub qos-installer (--stub-ui: OS-plumbing test, no UI)"
  cat > "$QOS_OS_DIR/qos-installer" <<'STUB'
#!/bin/bash
# Placeholder qOS installer (built with --stub-ui). The real UI is omitted;
# this exists only to exercise the OS plumbing (session-select, sudoers,
# first-boot units). It stays alive so cage keeps the session up.
echo "qos-installer STUB (--stub-ui): UI omitted; OS plumbing test only."
echo "kernel cmdline: $(cat /proc/cmdline 2>/dev/null)"
exec sleep infinity
STUB
  chmod 755 "$QOS_OS_DIR/qos-installer"
elif [ "$DO_CARGO" = 1 ]; then
  msg "1/3 cargo build --release -p installer in ${QOS_DISTROBOX:-host}"
  inenv "cd '$QOS_REPO' && cargo build --release -p installer"
  REL="$QOS_REPO/target/release"
  [ -f "$REL/qos-installer" ] || die "missing built binary: $REL/qos-installer"
  cp -f "$REL/qos-installer" "$QOS_OS_DIR/qos-installer"
  msg "   staged qos-installer ($(du -h "$REL/qos-installer" | cut -f1))"
else
  msg "1/3 skipping cargo (--no-cargo); reusing $QOS_OS_DIR/qos-installer"
  [ -f "$QOS_OS_DIR/qos-installer" ] || die "no $QOS_OS_DIR/qos-installer present (drop --no-cargo)"
fi

msg "2/3 podman build $QOS_INSTALLER_IMAGE (rootless)"
# Assemble one exact rootfs overlay so the Containerfile needs a single COPY
# layer. On overlay-backed bootc images, committing every tiny file as its own
# layer repeatedly scans the multi-gigabyte base and turns an iterative
# installer build into an hour-long job.
INSTALLER_SCRATCH="$TMPDIR"
INSTALLER_CONTEXT="$(mktemp -d "$INSTALLER_SCRATCH/qos-installer-context.XXXXXX")"
cleanup_installer_context() { rm -rf "$INSTALLER_CONTEXT"; }
trap cleanup_installer_context EXIT
mkdir -p "$INSTALLER_CONTEXT/rootfs/usr" \
         "$INSTALLER_CONTEXT/rootfs/etc/greetd" \
         "$INSTALLER_CONTEXT/rootfs/usr/share/qos"
cp -a "$QOS_OS_DIR/usr/." "$INSTALLER_CONTEXT/rootfs/usr/"
install -Dm0755 "$QOS_OS_DIR/qos-installer" \
  "$INSTALLER_CONTEXT/rootfs/usr/bin/qos-installer"
install -Dm0755 "$QOS_OS_DIR/dev/uki-cmdline.py" \
  "$INSTALLER_CONTEXT/rootfs/usr/libexec/qos-uki-cmdline"
install -Dm0644 "$QOS_OS_DIR/etc/greetd/config.toml" \
  "$INSTALLER_CONTEXT/rootfs/usr/share/qos/greetd-config.toml"
install -Dm0644 "$QOS_OS_DIR/etc/greetd/config.installer.toml" \
  "$INSTALLER_CONTEXT/rootfs/etc/greetd/config.toml"
podman build \
  --build-arg "QOS_INSTALLER_DEV_SSH=$DEV_LOGIN" \
  -t "$QOS_INSTALLER_IMAGE" -f "$QOS_OS_DIR/Containerfile.installer" "$INSTALLER_CONTEXT"
cleanup_installer_context
trap - EXIT

msg "3/3 disk image -> installer qcow2"
# The live root holds both the booted composefs tree and a second unpacked image
# in /var/lib/containers (`bootc image copy-to-storage`) before `bootc install`.
# Size it from the actual image rather than a stale guess. Three times the OCI
# image size covers both trees plus overlay metadata; 40 GiB is the tested floor
# for the full Windows-compatibility payload. The pristine qcow2 remains sparse.
#
# `qos.install` is not passed here — it lives in the image, as
# /usr/lib/bootc/kargs.d/50-installer.toml (see os/Containerfile.installer).
rm -rf "$QOS_INSTALLER_OUTPUT/qcow2" "$QOS_INSTALLER_OUTPUT"/manifest* 2>/dev/null || true
image_bytes="$(podman image inspect "$QOS_INSTALLER_IMAGE" --format '{{.Size}}')"
gib=$((1024 * 1024 * 1024))
root_gib=$(((image_bytes * 3 + gib - 1) / gib))
[ "$root_gib" -ge 40 ] || root_gib=40
msg "   live root ${root_gib}G (OCI image $(numfmt --to=iec-i --suffix=B "$image_bytes"), 3x + 40G floor)"
DISK_ARGS=(
  "$QOS_INSTALLER_IMAGE"
  "$(dirname "$QOS_INSTALLER_DISK")"
  --root-min "${root_gib}G"
)
if [ "$DEV_LOGIN" = 1 ]; then
  DISK_ARGS+=(--user "quickuser:${QOS_INSTALLER_DEV_PASSWORD}:wheel")
fi
INSTALL_LOG="$(dirname "$QOS_INSTALLER_DISK")/.install.log"
if ! "$QOS_OS_DIR/dev/disk-image.sh" "${DISK_ARGS[@]}"; then
  want="$(sed -n "s/.*should be sha512:\([0-9a-f]\{128\}\).*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
  [ -n "$want" ] || die "the installer disk-image step failed for a reason other than the
  composefs seal; see the message above and $INSTALL_LOG"
  had="$(sed -n "s/.*is 'sha512:\([0-9a-f]\{128\}\)'.*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
  msg "bootc#2334: the live UKI sealed sha512:${had:0:32}… but install wants sha512:${want:0:32}…"
  msg "pass 2: rewriting that digest into the live UKI and retrying"
  "$QOS_OS_DIR/dev/reseal-image.sh" "$QOS_INSTALLER_IMAGE" "$want" \
    || die "could not rewrite the live installer's sealed composefs digest"
  if ! "$QOS_OS_DIR/dev/disk-image.sh" "${DISK_ARGS[@]}"; then
    want2="$(sed -n "s/.*should be sha512:\([0-9a-f]\{128\}\).*/\1/p" "$INSTALL_LOG" 2>/dev/null | tail -1)"
    if [ -n "$want2" ] && [ "$want2" != "$want" ]; then
      die "the digest install expects MOVED between passes (${want:0:16}… -> ${want2:0:16}…),
  even though only the UKI under /boot changed. Sealing is circular on this bootc
  version and requires an upstream fix."
    fi
    die "the second installer image pass failed too; see above and $INSTALL_LOG"
  fi
  msg "live installer seal closed in two passes"
fi

msg "DONE: $QOS_INSTALLER_DISK"
ls -lh "$QOS_INSTALLER_DISK"
