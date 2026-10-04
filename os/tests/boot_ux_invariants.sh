#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
uki="$root/os/dev/uki.sh"
kargs="$root/os/usr/lib/bootc/kargs.d/03-boot.toml"
containerfile="$root/os/Containerfile"
theme="$root/os/usr/share/plymouth/themes/tilo/tilo.plymouth"
script="$root/os/usr/share/plymouth/themes/tilo/tilo.script"

grep -Fq -- '--splash /out/tilo-splash.bmp' "$uki"
grep -Fq 'make-boot-splash.py /out/tilo-splash.bmp' "$uki"
# Check the installed path, including grouped COPY instructions.
# shellcheck source=lib/containerfile.sh
. "$root/os/tests/lib/containerfile.sh"
if ! containerfile_has_copy "$containerfile" os/dev/make-boot-splash.py /usr/local/bin/make-boot-splash.py; then
  printf 'image must install the boot splash generator at its runtime path\n' >&2
  exit 1
fi
grep -Fq 'plymouth-plugin-script' "$containerfile"
if ! grep -Fq -- '--add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback"' "$root/os/scripts/build-initramfs.sh" \
  || ! grep -Fq 'bash /run/tilo-build-initramfs.sh "$kver"' "$containerfile"; then
  printf 'initramfs must explicitly include crypt, TPM, Plymouth, and Tilo rollback unlock modules\n' >&2
  exit 1
fi
grep -Fq 'plymouth-set-default-theme tilo' "$containerfile"
grep -Fq 'ModuleName=script' "$theme"
grep -Fq 'Plymouth.SetDisplayPasswordFunction(display_password)' "$script"
grep -Fq 'if (!global.dialog)' "$script"
grep -Fq 'global.dialog.heading = Sprite' "$script"
grep -Fq 'global.dialog.entry = Sprite' "$script"
grep -Fq 'global.dialog.ready = 1' "$script"
if grep -Eq '^[[:space:]]+dialog\.(heading|help|entry|lock|bullet_image)[[:space:]]*=' "$script"; then
  printf 'Plymouth dialog state must be assigned globally from callbacks\n' >&2
  exit 1
fi
if grep -Fq 'plymouth-quit.service' "$containerfile"; then
  printf 'Plymouth quit services must not be masked\n' >&2
  exit 1
fi

for arg in quiet splash plymouth.ignore-serial-consoles loglevel=3 systemd.show_status=auto rd.udev.log_level=3 vt.global_cursor_default=0; do
  grep -Fq "\"$arg\"" "$kargs" || {
    printf 'missing boot UX argument: %s\n' "$arg" >&2
    exit 1
  }
done

mkdir -p "$root/target"
tmp="$(mktemp -d "$root/target/boot-ux.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
python3 "$root/os/dev/make-boot-splash.py" "$tmp/splash.bmp"
python3 "$root/os/dev/make-plymouth-assets.py" "$tmp/plymouth"
python3 - "$tmp/splash.bmp" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
assert data[:2] == b"BM"
assert struct.unpack_from("<ii", data, 18) == (1024, 768)
assert len(set(data[54:])) > 24, "splash is visually empty"
PY
grep -Fq "Tilo's three-dot mark" "$root/os/dev/make-boot-splash.py"
grep -Fq 'for index, center_x in enumerate((56.0, 110.0, 164.0))' \
  "$root/os/dev/make-plymouth-assets.py"
if grep -Fq 'recognisable q' "$root/os/dev/make-boot-splash.py" \
   || grep -Fq 'tail_distance' "$root/os/dev/make-plymouth-assets.py"; then
  printf 'boot artwork still carries the retired Tilo q mark\n' >&2
  exit 1
fi
python3 - "$tmp/plymouth" <<'PY'
import pathlib, struct, sys
root = pathlib.Path(sys.argv[1])
expected = {"tilo-logo.png", "entry.png", "lock.png", "bullet.png",
            "unlock-heading.png", "unlock-help.png", "unlock-retry.png",
            "progress-background.png", "progress-fill.png"}
assert {path.name for path in root.iterdir()} == expected
for path in root.iterdir():
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    width, height = struct.unpack(">II", data[16:24])
    assert width > 0 and height > 0, path
PY

printf 'boot UX invariants hold\n'
