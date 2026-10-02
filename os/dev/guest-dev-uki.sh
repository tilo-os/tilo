#!/usr/bin/env bash
# Exercise systemd-stub using the running dev disk's own kernel, initrd and
# command line. This changes only a disposable VM overlay, not an OS image.
set -euo pipefail
[[ $(uname -m) == x86_64 ]] || { echo 'dev UKI diagnostic requires x86_64' >&2; exit 2; }
stub=/usr/lib/systemd/boot/efi/linuxx64.efi.stub
test -s "$stub"
work=$(mktemp -d /var/tmp/tilo-dev-uki.XXXXXX)
trap 'rm -rf -- "$work"' EXIT
python3 - "$work" <<'PY'
import pathlib, re, sys
work = pathlib.Path(sys.argv[1])
args = pathlib.Path('/proc/cmdline').read_text().split()
digests = [a.split('=', 1)[1] for a in args if a.startswith('composefs=')]
if len(digests) != 1 or not re.fullmatch('[0-9a-f]{128}', digests[0]):
    raise SystemExit('expected one running dev composefs digest')
kernel_dir = pathlib.Path('/boot/EFI/Linux') / ('bootc_composefs-' + digests[0])
for name in ('vmlinuz', 'initrd'):
    if not (kernel_dir / name).is_file():
        raise SystemExit('running dev kernel or initrd missing')
(work / 'kernel-dir').write_text(str(kernel_dir))
(work / 'cmdline').write_bytes((' '.join(a for a in args if not a.startswith('initrd=')) + '\0').encode())
PY
kernel_dir=$(cat "$work/kernel-dir")
cp /usr/lib/os-release "$work/osrel"
uname -r | tr -d '\n' > "$work/uname"
mount -o remount,rw /boot
initrd_vma=$(printf '0x%x' "$(( (0x2000000 + $(stat -c %s "$kernel_dir/vmlinuz") + 0xfffff) & ~0xfffff ))")
objcopy \
  --add-section .osrel="$work/osrel" --change-section-vma .osrel=0x20000 \
  --add-section .cmdline="$work/cmdline" --change-section-vma .cmdline=0x30000 \
  --add-section .uname="$work/uname" --change-section-vma .uname=0x40000 \
  --add-section .linux="$kernel_dir/vmlinuz" --change-section-vma .linux=0x2000000 \
  --add-section .initrd="$kernel_dir/initrd" --change-section-vma .initrd="$initrd_vma" \
  "$stub" /boot/EFI/Linux/tilo-dev-diagnostic.efi
objdump -h /boot/EFI/Linux/tilo-dev-diagnostic.efi
bootctl set-oneshot tilo-dev-diagnostic.efi
sync
