#!/usr/bin/env bash
# The release gate's SSH access stays validation-only: a signed extension and
# /etc edits confined to the gate's own disk copy, never the image.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
access="$repo/os/dev/validation-access.sh"
disk="$repo/os/dev/disk-image.sh"
vm="$repo/os/dev/vm.sh"
build="$repo/os/dev/build-image.sh"
workflow="$repo/.forgejo/workflows/build-image.yml"
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

# Only a generic publication's gate prepares it; build-image.sh passes it only
# when asked; the image still carries no SSH server.
if grep -Fq 'if [[ $PUBLISH == true && $HARDWARE_PROFILE == generic ]]; then' "$workflow" \
   && grep -Fq './os/dev/validation-access.sh prepare "$TILO_RUN/validation" "$TILO_IMAGE" "$RPMS_REF"' "$workflow" \
   && grep -Fq 'if [[ $PUBLISH == true && $HARDWARE_PROFILE == generic && '\''${{ matrix.arch }}'\'' == aarch64 ]]; then' "$workflow" \
   && grep -Fq './os/tests/validation_drive_qemu.sh' "$workflow" \
   && grep -Fq 'disk_args+=(--etc-overlay "$TILO_VALIDATION_ETC")' "$build" \
   && grep -Fq 'openssh-server' "$repo/os/scripts/verify-runtime-crypto-packages.sh" \
   && ! grep -Eq 'dnf install[^&]*openssh-server' "$image"; then
  ok 'validation access is prepared for publication gates only; the image stays sshd-free'
else
  bad 'validation access leaks beyond the publication gate, or the image gained sshd'
fi

exit "$fail"
