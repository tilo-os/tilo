#!/usr/bin/env bash
# The release gate's SSH access stays validation-only: a signed extension and
# /etc edits confined to the gate's own disk copy, never the image.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
access="$repo/os/dev/validation-access.sh"
disk="$repo/os/dev/disk-image.sh"
vm="$repo/os/dev/vm.sh"
build="$repo/os/dev/build-image.sh"
image="$repo/os/Containerfile"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# disk-image.sh refuses, before touching any disk, an overlay that would write
# anything but the validation unit, its trust anchor and sshd's PAM stack.
refuses() {
  local name=$1 expect=$2; shift 2
  local overlay="$work/$name"
  mkdir -p "$overlay"
  "$@" "$overlay"
  out="$("$disk" localhost/tilo-test "$work/out-$name" --etc-overlay "$overlay" 2>&1)"
  if [ $? -ne 0 ] && grep -Fq -- "$expect" <<<"$out"; then
    ok "disk-image.sh refuses an overlay that $name"
  else
    bad "disk-image.sh accepted an overlay that $name: $out"
  fi
}
refuses 'writes /etc/shadow' 'may not write /etc/shadow' \
  sh -c 'echo x > "$0/shadow"'
refuses 'adds an arbitrary unit drop-in' 'may not write /etc/systemd/system/getty@.service.d/x.conf' \
  sh -c 'mkdir -p "$0/systemd/system/getty@.service.d" && echo x > "$0/systemd/system/getty@.service.d/x.conf"'
refuses 'replaces the sudo PAM stack' 'may not write /etc/pam.d/sudo' \
  sh -c 'mkdir -p "$0/pam.d" && echo x > "$0/pam.d/sudo"'
refuses 'links the diagnostic script outside the overlay' 'diagnostics script must be a regular file' \
  sh -c 'ln -s /etc/passwd "$0/tilo-validation-diagnostics.sh"'
refuses 'holds a device node' 'contains a special file' \
  sh -c 'mkdir -p "$0/verity.d" && mkfifo "$0/verity.d/x.crt"'

# The overlay is added without rewriting the directories it lands in: `cp -a
# SRC/. DST/` copied the runner work directory's label onto the deployment's
# /etc (var_t), which broke every confined service that writes there. The build
# compares label, mode and owner of /etc and each existing parent before and
# after, and fails on any change.
if grep -Fq -- '--no-overwrite-dir' "$disk" \
   && ! grep -Fq 'cp -a --no-preserve=ownership "$ETC_OVERLAY/."' "$disk" \
   && grep -Fq 'os.getxattr(path, "security.selinux")' "$disk" \
   && grep -Fq 'die "the validation overlay changed the label, mode or owner of an existing /etc directory"' "$disk"; then
  ok 'the validation overlay leaves existing /etc directories, and their labels, as they were'
else
  bad 'the validation overlay can rewrite the label or mode of the deployment /etc'
fi

# The release gate's diagnostic service must pass the overlay policy. Use an
# intentionally missing helper to stop at the next preflight without a build.
allowed_overlay="$work/diagnostics-allowed"
mkdir -p "$allowed_overlay"
printf '%s\n' 'printf diagnostics' > "$allowed_overlay/tilo-validation-diagnostics.sh"
out="$("$disk" localhost/tilo-test "$work/out-diagnostics" \
  --etc-overlay "$allowed_overlay" --bootc-helper "$work/missing-helper" 2>&1)"
if [ $? -ne 0 ] && grep -Fq -- '--bootc-helper must name an executable regular, non-symlink file' <<<"$out"; then
  ok 'the validation diagnostic script passes overlay preflight'
else
  bad "the validation diagnostic script did not pass overlay preflight: $out"
fi

# The unit merges only a signed root partition and binds to the disk vm.sh
# attaches; the device unit name is the systemd escape of its by-id path.
escaped="$(python3 -c 'print("dev-" + "disk/by-id/virtio-tilo-validation".replace("-", "\\x2d").replace("/", "-") + ".device")')"
if grep -Fxq "BindsTo=$escaped" "$access" \
   && grep -Fq -- '--image-policy=root=signed+absent:usr=absent:=unused+absent' "$access" \
   && grep -Fq 'ExecStartPre=/usr/bin/env SYSTEMD_LOG_LEVEL=debug /usr/bin/systemd-sysext refresh' "$access" \
   && ! grep -Fq 'Environment=SYSTEMD_LOG_LEVEL=debug' "$access" \
   && grep -Fq 'ExecStartPre=/usr/bin/test -r /etc/verity.d/tilo-validation.crt' "$access" \
   && grep -Fxq 'After=systemd-update-done.service' "$access" \
   && grep -Fq 'TILO_VALIDATION_CERT_MISSING' "$access" \
   && grep -Fq 'format=raw,if=none,id=tilo-validation,readonly=on' "$vm" \
   && grep -Fq -- '-device virtio-blk-pci,drive=tilo-validation,serial=tilo-validation' "$vm" \
   && grep -Fq 'ln -sfn /dev/disk/by-id/virtio-tilo-validation /run/extensions/tilo-validation.raw' "$access"; then
  ok 'the validation unit merges only the signed extension on the disk vm.sh attaches'
else
  bad 'the validation unit can merge an unsigned extension or misses its disk'
fi

# The signing key lives for one image; only its certificate is kept, in the
# gate copy's /etc/verity.d.
if grep -Fq 'rm -f /work/validation.key' "$access" \
   && grep -Fq '[ ! -e "$work/validation.key" ] || die' "$access" \
   && grep -Fq 'install -D -m 0644 "$work/validation.crt" "$etc/verity.d/$EXTENSION.crt"' "$access" \
   && ! grep -Eq 'verity\.d|validation\.crt' "$image"; then
  ok 'the validation key is discarded and its certificate reaches only the gate disk'
else
  bad 'the validation signing key can outlive the gate, or its trust can reach the image'
fi

# A virtio-backed raw block device can have a different backing name from the
# symlink in /run/extensions. systemd 259 checks extension-release against that
# backing name while reading metadata, before it merges the sysext. Ensure the
# documented single-file filename fallback is embedded in the signed EROFS.
if python3 - "$access" <<'PY'
from pathlib import Path
import sys

source = Path(sys.argv[1]).read_text()
release = '/tree/usr/lib/extension-release.d/extension-release.$EXTENSION'
create = source.index('} > ' + release)
set_xattr = source.index('setfattr -n user.extension-release.strict -v 0')
verify = source.index('getfattr --only-values -n user.extension-release.strict')
mkfs = source.index('mkfs.erofs --quiet --file-contexts=')
assert create < set_xattr < verify < mkfs
assert source.count('} > ' + release) == 1
assert '  attr cpio cryptsetup-libs erofs-utils openssl systemd-repart ' in source
assert '--image-policy=root=signed+absent:usr=absent:=unused+absent' in source
PY
then
  ok 'signed validation DDI carries the single-file extension-release fallback before EROFS sealing'
else
  bad 'signed validation DDI can fail extension-release lookup on a virtio block device'
fi

# build-image.sh passes the validation overlay only when asked; the image still
# carries no SSH server.
if grep -Fq 'disk_args+=(--etc-overlay "$TILO_VALIDATION_ETC")' "$build" \
   && grep -Fq 'openssh-server' "$repo/os/scripts/verify-runtime-crypto-packages.sh" \
   && ! grep -Eq 'dnf install[^&]*openssh-server' "$image"; then
  ok 'validation access reaches only a disk built with it; the image stays sshd-free'
else
  bad 'validation access leaks beyond the gate disk, or the image gained sshd'
fi

exit "$fail"
