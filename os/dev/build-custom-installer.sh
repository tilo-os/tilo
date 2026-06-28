#!/bin/bash
# Build the qOS CUSTOM (self-install) installer: a bootable live qcow2 that runs
# the qOS-branded graphical installer (installer/, binary qos-installer) under
# cage and installs the booted image onto a target disk. This is SEPARATE from
# the Anaconda ISO path (build-installer.sh / installer.toml).
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
#   3. `podman save | sudo podman load` into root storage for bootc-image-builder.
#   4. bootc-image-builder --type qcow2 --rootfs btrfs, with installer-disk.toml
#      (bakes qos.installer=1) -> os/output/installer/qcow2/disk.qcow2.
#
# Flags: --stub-ui (no cargo; placeholder UI)  --no-cargo (reuse os/qos-installer)
source "$(dirname "${BASH_SOURCE[0]}")/_common.sh"

STUB_UI=0
DO_CARGO=1
for a in "$@"; do
  case "$a" in
    --stub-ui) STUB_UI=1; DO_CARGO=0 ;;
    --no-cargo) DO_CARGO=0 ;;
    -h|--help) sed -n '2,40p' "$0"; exit 0 ;;
    *) die "unknown arg: $a" ;;
  esac
done

# Containerfile.installer is `FROM localhost/qos:latest`, so the base desktop
# image must already exist in ROOTLESS podman storage (the build is rootless).
podman image exists "$QOS_IMAGE" \
  || die "$QOS_IMAGE not in rootless podman storage — run ./os/dev/build-image.sh first"

if [ "$STUB_UI" = 1 ]; then
  msg "1/4 writing stub qos-installer (--stub-ui: OS-plumbing test, no UI)"
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
  msg "1/4 cargo build --release -p installer in ${QOS_DISTROBOX:-host}"
  inenv "cd '$QOS_REPO' && cargo build --release -p installer"
  REL="$QOS_REPO/target/release"
  [ -f "$REL/qos-installer" ] || die "missing built binary: $REL/qos-installer"
  cp -f "$REL/qos-installer" "$QOS_OS_DIR/qos-installer"
  msg "   staged qos-installer ($(du -h "$REL/qos-installer" | cut -f1))"
else
  msg "1/4 skipping cargo (--no-cargo); reusing $QOS_OS_DIR/qos-installer"
  [ -f "$QOS_OS_DIR/qos-installer" ] || die "no $QOS_OS_DIR/qos-installer present (drop --no-cargo)"
fi

msg "2/4 podman build $QOS_INSTALLER_IMAGE (rootless)"
podman build -t "$QOS_INSTALLER_IMAGE" -f "$QOS_OS_DIR/Containerfile.installer" "$QOS_OS_DIR"

msg "3/4 transfer image to root storage"
podman save "$QOS_INSTALLER_IMAGE" | sudo podman load

msg "4/4 bootc-image-builder -> installer qcow2"
mkdir -p "$QOS_INSTALLER_OUTPUT"
rm -rf "$QOS_INSTALLER_OUTPUT/qcow2" "$QOS_INSTALLER_OUTPUT"/manifest* 2>/dev/null || true
sudo podman run --rm --privileged \
  --security-opt label=type:unconfined_t \
  -v "$QOS_OS_DIR/installer-disk.toml:/config.toml:ro" \
  -v "$QOS_INSTALLER_OUTPUT:/output" \
  -v /var/lib/containers/storage:/var/lib/containers/storage \
  "$QOS_BIB" \
  --type qcow2 --rootfs btrfs --chown "$(id -u):$(id -g)" \
  "$QOS_INSTALLER_IMAGE"

msg "DONE: $QOS_INSTALLER_DISK"
ls -lh "$QOS_INSTALLER_DISK"
