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
# Names and messages on the boot sign-in screen are drawn by the label plugin.
grep -Fq 'plymouth-plugin-label' "$containerfile"
grep -Fq 'Image.Text(' "$script"
if ! grep -Fq -- '--add "crypt systemd-cryptsetup tpm2-tss plymouth tilo-rollback tilo-sibling-unlock"' "$root/os/scripts/build-initramfs.sh" \
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
assert "draw_signin()" in hotplug, \
    "a display change must lay the sign-in screen out again too"
assert "tilo-signin-" in body("display_message"), \
    "the theme must take the sign-in screen's messages"
assert '"tilo-otherways:"' in body("display_message"), \
    "the theme must say how to go back to signing in from another way in"
assert '"tilo-unlock-ask:"' in body("display_message"), \
    "the theme must say what the volume's prompt asks for"
# Every message tilo-boot-signin sends is one the theme reads.
signin = body("signin_message")
for key in ("people", "person", "choose", "face", "name", "account", "kind", "back",
            "note", "checking", "end"):
    assert f'key == "{key}"' in signin, f"tilo.script ignores tilo-signin-{key}"
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
look="$root/os/usr/share/plymouth/themes/tilo/look.json"
python3 "$root/os/dev/make-plymouth-assets.py" "$tmp/plymouth" "$look"
# Plymouth's script language reads no files: the login screen's look sits in
# tilo.script itself, and must be the look.json the greeter's test keeps.
python3 "$root/os/dev/make-plymouth-assets.py" --script-look "$look" >"$tmp/look.block"
python3 - "$script" "$tmp/look.block" <<'PY'
import sys
script, block = (open(path).read() for path in sys.argv[1:])
assert block in script, \
    "tilo.script's look is not look.json's: paste make-plymouth-assets.py --script-look over it"
PY
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
expected = {"tilo-logo.png", "entry.png", "lock.png", "bullet.png",
            "unlock-heading.png", "unlock-help.png", "unlock-retry.png",
            "starting-tilo.png", "shutting-down.png", "startup-help.png",
            "progress-background.png", "progress-fill.png",
            # The sign-in screen (tilo.script, "Sign-in").
            "avatar-person.png", "signin-heading.png", "field.png", "field-invalid.png",
            "field-bullet.png", "alert.png", "placeholder-pin.png", "placeholder-password.png",
            "note-wrong-pin.png", "note-wrong-password.png", "note-wait.png",
            "signing-in.png", "hint-other-ways.png", "hint-someone-else.png", "hint-go-back.png",
            "list-heading.png", "list-keys.png", "row.png", "row-selected.png",
            "cant-sign-in.png", "account-1.png", "account-2.png", "account-3.png",
            "account-4.png"} | {f"avatar-{slot}.png" for slot in range(12)} \
         | {f"unlock-help-{kind}.png" for kind in ("password-recovery", "passphrase-recovery",
                                                   "password", "passphrase", "recovery")}
assert {path.name for path in root.iterdir()} == expected
# Every image the theme loads by name is one the build renders.
loaded = set(re.findall(r'Image\("([^"]+\.png)"\)', open(sys.argv[2]).read()))
assert loaded and loaded <= expected, sorted(loaded - expected)
for path in root.iterdir():
    data = path.read_bytes()
    assert data[:8] == b"\x89PNG\r\n\x1a\n", path
    width, height = struct.unpack(">II", data[16:24])
    assert width > 0 and height > 0, path
PY

printf 'boot UX invariants hold\n'
