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
# The sign-in screen is the greeter (docs/security/sign-in.md §3.4): the theme
# draws only pre-rendered text, so neither Plymouth's label plugin nor a face
# for it, nor the sign-in layout and its messages, come back.
if grep -Fq 'plymouth-plugin-label' "$containerfile" \
  || grep -Eq 'Image\.Text|tilo-signin-|signin\.' "$script" \
  || [ -e "$root/os/usr/lib/dracut/modules.d/93tilo-boot-face" ]; then
  printf 'the boot screen must not draw a sign-in of its own; the greeter is the sign-in screen\n' >&2
  exit 1
fi
if ! grep -Fq -- '--add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock tilo-greeter"' "$root/os/scripts/build-initramfs.sh" \
  || ! grep -Fq 'bash /run/tilo-build-initramfs.sh "$kver"' "$containerfile"; then
  printf 'initramfs must explicitly include crypt, TPM, Plymouth, Tilo rollback unlock and greeter modules\n' >&2
  exit 1
fi
# The sign-in screen reads touch screens and speaks: their drivers come along,
# the sound drivers load (snd-seq, which loading snd-pcm loads), and ALSA's
# default device is not routed to a PipeWire that never runs there.
greeter_module="$root/os/usr/lib/dracut/modules.d/94tilo-greeter/module-setup.sh"
if ! grep -Fq "instmods =drivers/input/touchscreen virtio_input" "$greeter_module" \
  || ! grep -Fq "=sound/virtio snd-hda-intel snd-seq" "$greeter_module" \
  || ! grep -Fq 'echo "audio:x:${audio:-63}:" >> "${initdir:?}/etc/group"' "$greeter_module" \
  || ! grep -Fq "find /usr/share/alsa -type f -not -path '/usr/share/alsa/alsa.conf.d/*'" "$greeter_module"; then
  printf 'the boot sign-in must carry touch screen and sound drivers and an ALSA default that opens without PipeWire\n' >&2
  exit 1
fi
grep -Fq 'plymouth-set-default-theme tilo' "$containerfile"
grep -Fq 'ModuleName=script' "$theme"
grep -Fq 'Plymouth.SetDisplayPasswordFunction(display_password)' "$script"
grep -Fq 'if (!global.dialog)' "$script"
grep -Fq 'global.dialog.heading = Sprite' "$script"
grep -Fq 'global.dialog.entry = Sprite' "$script"
grep -Fq 'global.dialog.ready = 1' "$script"
# The splash starts on the firmware framebuffer and moves to the GPU driver's
# native mode mid-boot. Positions computed for the first screen would leave
# the splash off-centre on the second, so a display change lays it out again.
grep -Fq 'Plymouth.SetDisplayHotplugFunction(display_hotplug)' "$script"
python3 - "$script" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
def body(name):
    match = re.search(r"^fun " + name + r"\(.*?^  }$", text, re.S | re.M)
    assert match, f"tilo.script has no {name}()"
    return match.group(0)
assert "place_dialog();" in body("display_password"), \
    "the unlock prompt must be positioned for the current screen each time it is shown"
assert "SetPosition" not in body("setup_dialog"), \
    "setup_dialog() creates sprites; place_dialog() positions them for the current screen"
hotplug = body("display_hotplug")
assert "display_password" in hotplug and "display_normal" in hotplug, \
    "a display change must re-run the layout of whichever screen is showing"
assert '"tilo-otherways:"' in body("display_message"), \
    "the theme must say how to go back to signing in from another way in"
assert '"tilo-unlock-ask:"' in body("display_message"), \
    "the theme must say what the volume's prompt asks for"
PY
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

scratch="${CARGO_TARGET_DIR:-$root/target}"
mkdir -p "$scratch"
tmp="$(mktemp -d "$scratch/boot-ux.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
python3 "$root/os/dev/make-boot-splash.py" "$tmp/splash.bmp"
# The image build draws in the login screen's family (Tilo Sans) from the font
# catalog. Where the catalog is not installed, this checks the theme's
# structure in the variable sans fontconfig has, cut the same way.
face_args=()
family="Tilo Sans"
grep -Fq "\"family\": \"$family\"" "$root/os/dev/make-plymouth-assets.py"
if ! fc-match -f '%{family}' "$family" | tr ',' '\n' | grep -Fxq "$family"; then
  face_args=(--face "$(fc-match -f '%{file}' 'sans-serif:slant=0')")
fi
python3 "$root/os/dev/make-plymouth-assets.py" "$tmp/plymouth" "${face_args[@]}"
python3 - "$tmp/splash.bmp" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
assert data[:2] == b"BM"
width, height = struct.unpack_from("<ii", data, 18)
assert (width, height) == (1024, 768)
assert len(set(data[54:])) > 24, "splash is visually empty"
# systemd-stub clears the screen to black and centres this tile at the
# firmware's mode; a non-black edge shows as a box on any larger panel.
stride = (width * 3 + 3) & ~3
def pixel(x, y):
    at = 54 + (height - 1 - y) * stride + x * 3
    return tuple(data[at:at + 3])
edge = [pixel(x, y) for x in range(width) for y in (0, height - 1)]
edge += [pixel(x, y) for y in range(height) for x in (0, width - 1)]
assert all(value == (0, 0, 0) for value in edge), \
    "the UKI splash tile must be black at its edge, like the stub's fill around it"
PY
# Both boot screens draw the brand's mark from the same path data, so the
# stub's tile and the Plymouth theme cannot drift apart.
python3 - "$root/os/dev/make-boot-splash.py" "$root/os/dev/make-plymouth-assets.py" <<'PY'
import sys
def mark(path):
    text = open(path).read()
    start = text.index("MARK_SIDES = (")
    return text[start:text.index("\n\n\n", start)]
splash, theme = (mark(path) for path in sys.argv[1:])
assert splash == theme, "the UKI splash and the Plymouth theme draw different marks"
assert "M10.98 31.85 Q10.979892448752274 27.96648782251284 14.4 26.12" in splash
assert "MARK_BLOCK = (29.0, 16.0, 71.0, 84.0, 6.0)" in splash
PY
if grep -Fq 'recognisable q' "$root/os/dev/make-boot-splash.py" \
   || grep -Fq 'tail_distance' "$root/os/dev/make-plymouth-assets.py" \
   || grep -Fq 'enumerate((56.0, 110.0, 164.0))' "$root/os/dev/make-plymouth-assets.py"; then
  printf 'boot artwork still carries a retired Tilo mark\n' >&2
  exit 1
fi
python3 - "$tmp/plymouth" "$script" <<'PY'
import pathlib, re, struct, sys
root = pathlib.Path(sys.argv[1])
expected = {"tilo-logo.png",
            "unlock-heading.png", "unlock-help.png", "unlock-retry.png",
            "starting-tilo.png", "shutting-down.png", "startup-help.png",
            "progress-background.png", "progress-fill.png",
            # The disk's prompt (tilo.script, "dialog").
            "field.png", "field-bullet.png", "hint-go-back.png"} \
         | {f"unlock-help-{kind}.png" for kind in ("password-recovery", "passphrase-recovery",
                                                   "password", "passphrase", "recovery")}
assert {path.name for path in root.iterdir()} == expected, \
    sorted({path.name for path in root.iterdir()} ^ expected)
# Every image the theme loads by name is one the build renders.
loaded = set(re.findall(r'Image\("([^"]+\.png)"\)', open(sys.argv[2]).read()))
assert loaded and loaded <= expected, sorted(loaded - expected)
for path in root.iterdir():
    if path.suffix != ".png":
        continue
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    width, height = struct.unpack(">II", data[16:24])
    assert width > 0 and height > 0, path
PY

printf 'boot UX invariants hold\n'
