#!/usr/bin/env bash
# Validation-only SSH access to a sealed release disk.
#
# The release image ships no SSH server (the runtime crypto policy forbids
# one), yet the release gate drives the installed system over `vm.sh ssh`.
# This prepares, for one gate run only:
#
#   OUTDIR/tilo-validation.raw  a system extension DDI carrying sshd from the
#                               frozen RPM snapshot, dm-verity protected and
#                               signed with a key generated here and discarded
#                               as soon as the image is signed
#   OUTDIR/etc/                 files for the gate's OWN copy of the installed
#                               disk (disk-image.sh --etc-overlay): the
#                               signing certificate in /etc/verity.d, a unit
#                               that merges exactly that extension with a
#                               signed-only image policy and starts sshd, and
#                               sshd's PAM stack
#   OUTDIR/etc-edits.txt        every file the overlay adds, with its SHA-256,
#                               for the gate log
#
# Nothing here touches the image, its UKI, its kernel command line or any
# published artifact. The sealed image merges no extension by itself
# (os/Containerfile masks systemd-sysext); the overlay's unit is what merges
# this one, and the certificate it trusts exists only in the gate's disk copy.
#
# Usage: os/dev/validation-access.sh prepare OUTDIR IMAGE RPMS_REF
#   IMAGE     the release image in rootless Podman storage
#   RPMS_REF  the frozen RPM snapshot that image was built from
set -euo pipefail

die() { printf 'validation-access: %s\n' "$*" >&2; exit 1; }

# Build tools only; nothing from this container reaches the extension except
# what it assembles from the snapshot's openssh-server RPM. The default is the
# digest-pinned Fedora base mirrored in our registry, which the image build has
# already pulled, not a public registry tag.
base_ref="$(sed -n 's/^BASE_REF=//p' "$(dirname "${BASH_SOURCE[0]}")/../base-snapshot.env")"
TOOLS_IMAGE="${TILO_VALIDATION_TOOLS_IMAGE:-$base_ref}"
EXTENSION=tilo-validation
UNIT=tilo-validation-ssh.service

[ "${1:-}" = prepare ] && [ $# -eq 4 ] \
  || die "usage: $0 prepare OUTDIR IMAGE RPMS_REF"
outdir=$2 image=$3 rpms_ref=$4
[[ $rpms_ref =~ @sha256:[0-9a-f]{64}$ ]] || die "RPMS_REF must be digest-pinned: $rpms_ref"
podman image exists "$image" || die "$image is not in rootless Podman storage"
podman image exists "$rpms_ref" || podman pull -q "$rpms_ref" >/dev/null
[[ $TOOLS_IMAGE =~ @sha256:[0-9a-f]{64}$ ]] || die "the tools image must be digest-pinned: $TOOLS_IMAGE"
podman image exists "$TOOLS_IMAGE" || podman pull -q "$TOOLS_IMAGE" >/dev/null

rm -rf "$outdir"
mkdir -p "$outdir/work"
work=$(cd "$outdir/work" && pwd)
outdir=$(cd "$outdir" && pwd)
trap 'rm -rf "$work"' EXIT

# The image's own identity and SELinux file contexts: the extension must match
# its os-release, and its files carry the labels the image's policy expects.
podman run --rm --network=none "$image" cat /usr/lib/os-release > "$work/os-release"
podman run --rm --network=none "$image" \
  cat /etc/selinux/targeted/contexts/files/file_contexts > "$work/file_contexts"
os_id=$(. "$work/os-release" && printf '%s' "$ID")
os_version=$(. "$work/os-release" && printf '%s' "${VERSION_ID:-}")
[ -n "$os_id" ] || die "the image's os-release has no ID"

cat > "$work/sshd_config" <<'EOF'
# Validation-only sshd for the CI release gate. Never part of a release.
Port 22
HostKey /etc/ssh/ssh_host_ed25519_key
PermitRootLogin no
PasswordAuthentication yes
KbdInteractiveAuthentication no
PubkeyAuthentication no
AuthorizedKeysFile none
UsePAM yes
PidFile /run/tilo-validation-sshd.pid
Subsystem sftp /usr/libexec/openssh/sftp-server
EOF

cat > "$work/build.sh" <<'EOF'
set -euo pipefail
dnf -q -y install --setopt=install_weak_deps=False \
  attr cpio cryptsetup-libs erofs-utils openssl systemd-repart >/dev/null
arch=$(uname -m)
shopt -s nullglob
rpms=(/rpms/repo/openssh-server-[0-9]*."$arch".rpm)
[ "${#rpms[@]}" -eq 1 ] || { echo "expected one openssh-server RPM for $arch in the snapshot" >&2; exit 1; }
mkdir /unpacked /tree
# Through a file: cpio stops at the archive trailer, and the SIGPIPE rpm2cpio
# then takes would fail the pipeline.
rpm2cpio "${rpms[0]}" > /payload.cpio
(cd /unpacked && cpio -idm --quiet < /payload.cpio)
# Only what sshd runs from /usr. Its units, man pages and /etc files stay out:
# the overlay's unit starts it with the configuration below.
for path in usr/bin/sshd usr/libexec/openssh/sshd-session usr/libexec/openssh/sshd-auth \
            usr/libexec/openssh/sftp-server usr/lib/sysusers.d/openssh-server.conf; do
  install -D -m "$(stat -c %a "/unpacked/$path")" "/unpacked/$path" "/tree/$path"
done
install -d -m 0711 /tree/usr/share/empty.sshd
install -D -m 0644 /work/sshd_config /tree/usr/lib/tilo-validation/sshd_config
install -d /tree/usr/lib/extension-release.d
{
  printf 'ID=%s\n' "$OS_ID"
  [ -z "$OS_VERSION" ] || printf 'VERSION_ID=%s\n' "$OS_VERSION"
} > /tree/usr/lib/extension-release.d/extension-release.$EXTENSION
# systemd 259 derives a block-backed image's metadata name from the backing
# device rather than /run/extensions/tilo-validation.raw. The raw disk arrives
# via virtio, so that device name is not stable. This documented xattr permits
# the one extension-release file above to match regardless of the device name;
# the image is still required to pass the signed-Verity policy at activation.
setfattr -n user.extension-release.strict -v 0 \
  /tree/usr/lib/extension-release.d/extension-release.$EXTENSION
test "$(getfattr --only-values -n user.extension-release.strict \
  /tree/usr/lib/extension-release.d/extension-release.$EXTENSION)" = 0
cp /unpacked/etc/pam.d/sshd /work/pam-sshd
# Label with the image's policy at mkfs time; no host relabel is involved.
mkfs.erofs --quiet --file-contexts=/work/file_contexts /work/root.erofs /tree
cp -a /tree /work/tree
mkdir /work/repart.d
cat > /work/repart.d/10-root.conf <<CONF
[Partition]
Type=root
CopyBlocks=/work/root.erofs
Verity=data
VerityMatchKey=root
CONF
cat > /work/repart.d/20-root-verity.conf <<CONF
[Partition]
Type=root-verity
Verity=hash
VerityMatchKey=root
Minimize=best
CONF
cat > /work/repart.d/30-root-verity-sig.conf <<CONF
[Partition]
Type=root-verity-sig
Verity=signature
VerityMatchKey=root
CONF
# An ephemeral key: it signs this one image and is deleted with /work.
openssl req -quiet -x509 -newkey rsa:3072 -nodes -days 3 \
  -subj "/CN=Tilo validation extension $(date -u +%Y%m%dT%H%M%SZ)" \
  -keyout /work/validation.key -out /work/validation.crt
systemd-repart --offline=yes --dry-run=no --empty=create --size=auto \
  --definitions=/work/repart.d --private-key=/work/validation.key \
  --certificate=/work/validation.crt /work/$EXTENSION.raw >/dev/null
rm -f /work/validation.key
EOF

podman run --rm --network=host \
  --mount "type=image,source=$rpms_ref,destination=/rpms" \
  -v "$work:/work:Z" \
  -e OS_ID="$os_id" -e OS_VERSION="$os_version" -e EXTENSION="$EXTENSION" \
  "$TOOLS_IMAGE" bash /work/build.sh
[ ! -e "$work/validation.key" ] || die "the signing key outlived the build"

# Every library sshd needs must already be in the image; the extension adds
# only sshd itself.
cat > "$work/ldd-check.sh" <<'EOF'
set -euo pipefail
for f in usr/bin/sshd usr/libexec/openssh/sshd-session usr/libexec/openssh/sshd-auth; do
  ldd "/work/tree/$f" | awk -v f="$f" '/=> not found/ { print f ": " $1 }'
done > /tmp/unresolved
[ ! -s /tmp/unresolved ] || { echo "the image lacks libraries sshd needs:" >&2; cat /tmp/unresolved >&2; exit 1; }
EOF
podman run --rm --network=none -v "$work:/work:ro,Z" "$image" bash /work/ldd-check.sh

mv "$work/$EXTENSION.raw" "$outdir/$EXTENSION.raw"

etc="$outdir/etc"
install -D -m 0644 "$work/validation.crt" "$etc/verity.d/$EXTENSION.crt"
install -D -m 0644 "$work/pam-sshd" "$etc/pam.d/sshd"
install -d "$etc/systemd/system/multi-user.target.wants"
# The device unit is the systemd escape of /dev/disk/by-id/virtio-tilo-validation,
# the serial vm.sh gives the extension disk.
cat > "$etc/systemd/system/$UNIT" <<'EOF'
# Validation-only SSH access, written into the CI gate's copy of the disk by
# os/dev/validation-access.sh. Never part of the image or a published disk.
[Unit]
Description=Validation-only SSH access for the release gate
BindsTo=dev-disk-by\x2did-virtio\x2dtilo\x2dvalidation.device
After=dev-disk-by\x2did-virtio\x2dtilo\x2dvalidation.device network.target basic.target
# The merge gives /usr a fresh mtime (systemd-sysext stamps it). After
# systemd-update-done, the boot's ConditionNeedsUpdate= checks (ldconfig,
# sysusers, hwdb) have all seen the image's own /usr, as on a user's boot.
After=systemd-update-done.service
# Off multi-user.target's ordering: a target waits for the units it wants
# unless they opt out, and this gate-only service would otherwise add its own
# sysext refresh and host-key generation to the boot time the gate measures.
DefaultDependencies=no
Conflicts=shutdown.target
Before=shutdown.target

[Service]
Type=exec
ExecStartPre=/usr/bin/test -r /etc/verity.d/tilo-validation.crt
ExecStartPre=/usr/bin/mkdir -p /run/extensions
ExecStartPre=/usr/bin/ln -sfn /dev/disk/by-id/virtio-tilo-validation /run/extensions/tilo-validation.raw
# Only a verity-signed root partition, verified against /etc/verity.d.
# Keep the signed-Verity diagnostics on this disposable gate disk: systemd's
# normal log level hides whether the userspace /etc/verity.d certificate
# fallback was attempted after kernel ENOKEY. Only for the refresh: sshd and
# every login it serves would otherwise inherit debug logging and flood the
# journal the gate's failure diagnostics print.
ExecStartPre=/usr/bin/env SYSTEMD_LOG_LEVEL=debug /usr/bin/systemd-sysext refresh --no-reload --image-policy=root=signed+absent:usr=absent:=unused+absent
ExecStartPre=/usr/bin/systemd-sysusers /usr/lib/sysusers.d/openssh-server.conf
ExecStartPre=/usr/bin/ssh-keygen -A
ExecStartPre=-/usr/sbin/restorecon -R /etc/ssh
ExecStart=/usr/bin/sshd -D -e -f /usr/lib/tilo-validation/sshd_config

[Install]
WantedBy=multi-user.target
EOF
ln -s "../$UNIT" "$etc/systemd/system/multi-user.target.wants/$UNIT"

# A boot can reach the serial login while greetd and validation SSH are both
# unavailable. Record bounded service state on the VM serial line in that case.
# This exists only in the gate's disk overlay, never in the release image.
cat > "$etc/tilo-validation-diagnostics.sh" <<'EOF'
#!/usr/bin/bash
sleep 45
# early once, then late every 30 s: the gate prints the serial tail when its
# SSH wait fails, about two to three minutes after boot, so the newest late
# report covers its connection attempts.
for phase in early late late late late late late late late; do
  report="$(
    echo "TILO_VALIDATION_DIAGNOSTICS $phase"
    systemctl is-system-running --no-pager || true
    systemctl list-jobs --no-pager || true
    systemctl --failed --no-pager || true
    systemctl status greetd.service tilo-validation-ssh.service \
      tilo-firstboot-user.service tilo-installer-setup.service \
      tilo-device-key-init.service --no-pager --lines=8 || true
    if [ -r /etc/verity.d/tilo-validation.crt ]; then
      echo "TILO_VALIDATION_CERT_PRESENT $(sha256sum /etc/verity.d/tilo-validation.crt)"
    else
      echo 'TILO_VALIDATION_CERT_MISSING'
    fi
    journalctl -b -u tilo-validation-ssh.service --no-pager -n 75 || true
    journalctl -b -u greetd.service -u tilo-validation-ssh.service \
      -u tilo-firstboot-user.service -u tilo-installer-setup.service \
      --no-pager -n 35 || true
    if [ "$phase" = late ]; then
      getent passwd tester || echo 'TILO_VALIDATION_NO_TESTER'
      faillock --user tester 2>&1 || true
      ss -tnlp 2>/dev/null | grep -E ':22\b' || echo 'TILO_VALIDATION_NOTHING_ON_22'
      ss -tn 2>/dev/null | grep -E ':22\b' || true
      journalctl -b --no-pager -o short-monotonic -n 25 \
        _COMM=sshd _COMM=sshd-session _COMM=sshd-auth _COMM=greetd _COMM=cage || true
    fi
    echo "TILO_VALIDATION_DIAGNOSTICS_END $phase"
  )"
  for serial in /dev/ttyS0 /dev/ttyAMA0; do
    if [ -c "$serial" ] && grep -q "^${serial##*/} " /proc/consoles; then
      printf '%s\n' "$report" | tail -n 160 > "$serial" || true
    fi
  done
  sleep 30
done
EOF
chmod 0644 "$etc/tilo-validation-diagnostics.sh"
cat > "$etc/systemd/system/tilo-validation-diagnostics.service" <<'EOF'
# Serial diagnostics for this gate's private installed disk copy only.
[Unit]
Description=Release boot gate diagnostics on the VM serial line
After=basic.target

[Service]
Type=simple
ExecStart=/usr/bin/bash /etc/tilo-validation-diagnostics.sh

[Install]
WantedBy=multi-user.target
EOF
ln -s ../tilo-validation-diagnostics.service \
  "$etc/systemd/system/multi-user.target.wants/tilo-validation-diagnostics.service"

(cd "$etc" && find . \( -type f -o -type l \) -printf '%P\n' | LC_ALL=C sort | while read -r f; do
  if [ -L "$f" ]; then
    printf 'etc/%s -> %s\n' "$f" "$(readlink "$f")"
  else
    printf 'etc/%s sha256:%s\n' "$f" "$(sha256sum "$f" | cut -d' ' -f1)"
  fi
done) > "$outdir/etc-edits.txt"
printf 'extension %s sha256:%s\n' "$EXTENSION.raw" \
  "$(sha256sum "$outdir/$EXTENSION.raw" | cut -d' ' -f1)" >> "$outdir/etc-edits.txt"
echo "validation access prepared in $outdir:"
cat "$outdir/etc-edits.txt"
